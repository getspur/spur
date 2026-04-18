# MCP Contract Truthfulness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore truthfulness at the MCP tool surface so every schema parameter either reaches execution or is rejected — no silent ignores, no error-reported-as-success.

**Architecture:** Three code layers changed in lockstep: (1) `tools.rs` JSON schemas; (2) `server.rs` handlers; (3) `orchestrator.rs::execute_delegation` prompt construction + `__*` stub guard. No changes outside `spur-mcp` and `spur-core`. TDD throughout; unit tests live inside `#[cfg(test)] mod tests` at the bottom of each source file unless otherwise noted.

**Tech Stack:** Rust (async/tokio), `serde_json`, `anyhow`. Test harness uses built-in `cargo test`.

**Source spec:** `docs/superpowers/specs/2026-04-18-mcp-contract-truthfulness-design.md`

---

## File map

| File | Responsibility | Role in plan |
|---|---|---|
| `crates/spur-mcp/src/tools.rs` | JSON-schema definitions + `tools_list()` registry | Modified by T1.1, T1.2, T1.3, T1.4 |
| `crates/spur-mcp/src/server.rs` | MCP JSON-RPC dispatch + handlers | Modified by T1.1, T1.2, T1.3, T1.4 |
| `crates/spur-core/src/orchestrator.rs` | `execute_delegation` + `__*` stub guard + new `format_worker_task` helper | Modified by T1.1, T1.2 |
| `crates/spur-mcp/tests/tool_catalog.rs` (new) | Snapshot test of `tools_list()` names (INV-1 guard) | Created by Task 1 |
| `crates/spur-acp/src/agents/defaults.rs` | Brain system-prompt defaults | Audited by Task 14 |

No test deletions in the main `tests/` dir — the prior behavior had no asserting tests for the broken cases (verified during MCTS round 2 of Q3).

---

## Dependencies across tasks

- Tasks 2–5 (T1.1 control-plane) are independent of Tasks 6–12 (T1.2/T1.3).
- Task 1 (tool-catalog test) must land before Tasks 3, 4 (which trip it deliberately then update it).
- Task 6 (`format_worker_task` helper) is a prerequisite for Task 7 (wiring it into `execute_delegation`).
- Task 9 (per-task `context_files` in parallel) and Task 10 (per-task `issue_id` + `delegation_plan`) touch the same handler loop; do them in order to avoid merge conflicts inside one function.

---

## Task 1: Tool catalog snapshot test (INV-1 guard)

Establish a baseline test that freezes the set of tool names. This trips when later tasks remove `report_progress` / `get_session_cost`, forcing the engineer to update the expected set explicitly rather than silently regressing.

**Files:**
- Create: `crates/spur-mcp/tests/tool_catalog.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-mcp/tests/tool_catalog.rs`:

```rust
//! Tool catalog snapshot test.
//!
//! Guards INV-1 from the T1 contract-truthfulness spec: the set of tool
//! names exposed via `tools/list` must not drift silently. Any addition
//! or removal requires updating the `EXPECTED` list in this test in the
//! same commit.

use spur_mcp::tools_list;

const EXPECTED: &[&str] = &[
    "delegate_to_worker",
    "delegate_parallel",
    "delegate_async",
    "wait_delegation",
    "check_delegation_status",
    "cancel_delegation",
    "list_available_workers",
    "get_issue",
    "list_issues",
    "update_issue",
    "create_issue",
    "add_dependency",
    "create_pr",
    "report_progress",
    "get_session_cost",
    "graph_triage",
    "graph_plan",
    "graph_insights",
    "graph_alerts",
    "graph_subgraph",
    "submit_plan",
    "execute_epic",
    "get_plan_status",
    "get_task_diff",
    "review_task",
];

#[test]
fn tool_catalog_matches_expected() {
    let actual: Vec<String> = tools_list().iter().map(|t| t.name.clone()).collect();
    let expected: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        actual, expected,
        "tool_catalog drift detected; update EXPECTED in tests/tool_catalog.rs if intentional",
    );
}
```

- [ ] **Step 2: Run test to verify it passes on current HEAD**

Run: `cargo test -p spur-mcp --test tool_catalog -- --nocapture`
Expected: PASS (current catalog matches EXPECTED verbatim).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/tests/tool_catalog.rs
git commit -m "test(spur-mcp): add tool-catalog snapshot guard (INV-1)"
```

---

## Task 2: Remove `source` parameter from `get_issue` and `update_issue` schemas (T1.4)

Smallest correctness fix — the field was already ignored by handlers; only the schema needs pruning.

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs:161-228`

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/spur-mcp/src/tools.rs`, inside an existing or new `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod schema_truthfulness_tests {
    use super::*;

    fn props_of(def: &ToolDefinition) -> Vec<String> {
        def.input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn get_issue_schema_does_not_advertise_source() {
        let def = get_issue_def();
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "get_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn update_issue_schema_does_not_advertise_source() {
        let def = update_issue_def();
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "update_issue must not advertise `source` until multi-backend lands",
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp schema_truthfulness -- --nocapture`
Expected: FAIL — both tests assert absence of `source`, which is currently present.

- [ ] **Step 3: Remove `source` from `get_issue_def()`**

In `crates/spur-mcp/src/tools.rs` around line 166, replace:

```rust
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "PM source override (github, linear, plane). Defaults to configured backend if omitted."
                },
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                }
            },
            "required": ["id"]
        }),
```

with:

```rust
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                }
            },
            "required": ["id"]
        }),
```

- [ ] **Step 4: Remove `source` from `update_issue_def()`**

Around line 234, replace the `properties` block's opening entry:

```rust
                "source": {
                    "type": "string",
                    "description": "PM source override (github, linear, plane). Defaults to configured backend if omitted."
                },
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                },
```

with:

```rust
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                },
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p spur-mcp schema_truthfulness -- --nocapture`
Expected: PASS.

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: PASS (tool names unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "refactor(spur-mcp): drop advertised-but-ignored source param from get_issue/update_issue

Closes RCA R7. Handlers never read the field; schema becomes honest.
Multi-backend routing returns via a future spec once PmService supports
more than one concurrent backend."
```

---

## Task 3: Remove `report_progress` tool (T1.1)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (remove `report_progress_def` + registry entry)
- Modify: `crates/spur-mcp/src/server.rs` (remove dispatch arm + handler)
- Modify: `crates/spur-mcp/tests/tool_catalog.rs` (update EXPECTED)

- [ ] **Step 1: Update EXPECTED in tool_catalog.rs to fail first**

In `crates/spur-mcp/tests/tool_catalog.rs`, remove the line `"report_progress",` from the `EXPECTED` array.

- [ ] **Step 2: Run catalog test to verify it fails**

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: FAIL — `report_progress` still present in `tools_list()` but missing from EXPECTED.

- [ ] **Step 3: Delete `report_progress_def()` from tools.rs**

Delete the entire function at `crates/spur-mcp/src/tools.rs:302-317`:

```rust
fn report_progress_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_progress".into(),
        description: "Report progress to the orchestrator (fire-and-forget).".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Progress message"
                }
            },
            "required": ["message"]
        }),
    }
}
```

- [ ] **Step 4: Remove registry entry**

In `tools_list()` at `crates/spur-mcp/src/tools.rs:783`, delete the line:

```rust
        report_progress_def(),
```

- [ ] **Step 5: Delete dispatch arm**

In `crates/spur-mcp/src/server.rs:427`, delete the line:

```rust
            "report_progress" => self.handle_report_progress(id, arguments).await,
```

- [ ] **Step 6: Delete handler function**

Delete the entire `handle_report_progress` function at `crates/spur-mcp/src/server.rs:1096-1124`:

```rust
    async fn handle_report_progress(&self, id: Value, args: Value) -> JsonRpcResponse {
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'message'"),
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let delegation = DelegationRequest {
            id: request_id,
            agent: "__progress".into(),
            task: message.clone(),
            context_files: Vec::new(),
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan: None,
            issue_id: None,
        };

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            warn!("Failed to send progress report");
        }

        info!(message = %message, "Progress reported");
        JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": "Progress reported." }] }),
        )
    }
```

- [ ] **Step 7: Run catalog test to verify it passes**

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: PASS.

- [ ] **Step 8: Run full spur-mcp tests**

Run: `cargo test -p spur-mcp`
Expected: PASS (no references to removed handler).

- [ ] **Step 9: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/tool_catalog.rs
git commit -m "refactor(spur-mcp): remove report_progress tool

Closes RCA R2/A2 for this tool. The handler forwarded to an
orchestrator stub (__progress) that returned Failed, and the MCP
response was a success-with-error-body. The only planned extension
(progress-milestone events) was removed in the stream-backbone plan
rev 2 because workers do not connect to the MCP server.

When a real brain-progress channel is wired, a new spec will reintroduce
an explicit tool."
```

---

## Task 4: Remove `get_session_cost` tool (T1.1)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/tests/tool_catalog.rs`

- [ ] **Step 1: Update EXPECTED in tool_catalog.rs**

In `crates/spur-mcp/tests/tool_catalog.rs`, remove the line `"get_session_cost",` from `EXPECTED`.

- [ ] **Step 2: Run catalog test to verify it fails**

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: FAIL.

- [ ] **Step 3: Delete `get_session_cost_def()` from tools.rs**

Delete the function at `crates/spur-mcp/src/tools.rs:319-328`:

```rust
fn get_session_cost_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_session_cost".into(),
        description: "Get the current cost breakdown for this session.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}
```

- [ ] **Step 4: Remove registry entry**

In `tools_list()`, delete the line:

```rust
        get_session_cost_def(),
```

- [ ] **Step 5: Delete dispatch arm**

In `crates/spur-mcp/src/server.rs:428`, delete:

```rust
            "get_session_cost" => self.handle_get_session_cost(id).await,
```

- [ ] **Step 6: Delete handler function**

Delete the entire `handle_get_session_cost` function at `crates/spur-mcp/src/server.rs:1126-1159`:

```rust
    async fn handle_get_session_cost(&self, id: Value) -> JsonRpcResponse {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let delegation = DelegationRequest {
            id: request_id,
            agent: "__session_cost".into(),
            task: String::new(),
            context_files: Vec::new(),
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan: None,
            issue_id: None,
        };

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, "Failed to forward request");
        }

        match rx.await {
            Ok(result) => {
                let text = result.summary.clone().unwrap_or_else(|| {
                    json!({ "estimated_cost_usd": result.estimated_cost_usd }).to_string()
                });
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(_) => JsonRpcResponse::internal_error(
                id,
                "get_session_cost failed: orchestrator disconnected",
            ),
        }
    }
```

- [ ] **Step 7: Run catalog test to verify it passes**

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: PASS.

- [ ] **Step 8: Run full spur-mcp tests**

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/tool_catalog.rs
git commit -m "refactor(spur-mcp): remove get_session_cost tool

Closes RCA R2/A2 for this tool. The handler returned 0.0 fallback
when orchestrator reported the __session_cost stub as Failed. Brain
can aggregate DelegationResult.estimated_cost_usd client-side if
cross-call cost is needed; cross-session analytics is a different
subsystem."
```

---

## Task 5: Fix `cancel_delegation` error-to-success inversion (T1.1, A2)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:735-815` (`handle_cancel_delegation`)

- [ ] **Step 1: Write the failing test**

Add at the bottom of `crates/spur-mcp/src/server.rs` inside `#[cfg(test)] mod cancel_delegation_tests` (create if absent):

```rust
#[cfg(test)]
mod cancel_delegation_tests {
    use super::*;
    use serde_json::json;
    use spur_acp::{DelegationResult, DelegationStatus};

    /// Simulated orchestrator response shape for the `__cancel_delegation`
    /// stub: status=Failed, summary=None. The test only exercises the
    /// pure translation from `DelegationResult` to `JsonRpcResponse` —
    /// extract this into a pure helper fn on McpCallbackServer so the
    /// test doesn't need a live channel.
    #[test]
    fn failed_result_maps_to_jsonrpc_error() {
        let id = json!(1);
        let result = DelegationResult {
            status: DelegationStatus::Failed {
                error: "Internal operation not yet wired: __cancel_delegation".into(),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
        };

        let resp = McpCallbackServer::cancel_result_to_response(id.clone(), result);

        // MUST be a JSON-RPC error, not success.
        assert!(
            resp.error.is_some(),
            "cancel_delegation stub-Failed must become JSON-RPC error, got success: {resp:?}",
        );
        let err = resp.error.as_ref().unwrap();
        assert_eq!(err.code, -32601);
        assert!(
            err.message.contains("cancel_delegation"),
            "error message should reference the tool: {}", err.message,
        );
    }

    #[test]
    fn successful_result_stays_success() {
        let id = json!(1);
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("cancelled".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
        };

        let resp = McpCallbackServer::cancel_result_to_response(id, result);
        assert!(resp.error.is_none(), "success result must stay success");
    }
}
```

- [ ] **Step 2: Run test to verify it fails (method doesn't exist)**

Run: `cargo test -p spur-mcp cancel_delegation -- --nocapture`
Expected: FAIL — `cancel_result_to_response` is undefined.

- [ ] **Step 3: Extract and fix the translation**

In `crates/spur-mcp/src/server.rs`, inside the `impl McpCallbackServer { ... }` block (adjacent to `handle_cancel_delegation`), add:

```rust
    /// Translate a `DelegationResult` from the orchestrator's `__cancel_delegation`
    /// stub into a JSON-RPC response. Extracted as a free function so it can
    /// be unit-tested without a live channel. When the status is `Failed`,
    /// the response is a JSON-RPC error (code -32601, "Method not
    /// implemented") carrying the orchestrator's error message. Any other
    /// status is surfaced as success; the body is `result.summary` when
    /// present, else a debug-rendered status.
    pub(crate) fn cancel_result_to_response(id: Value, result: DelegationResult) -> JsonRpcResponse {
        use spur_acp::DelegationStatus;
        if let DelegationStatus::Failed { ref error } = result.status {
            return JsonRpcResponse::error(
                id,
                -32601,
                format!("cancel_delegation: {error}"),
            );
        }
        let text = result.summary.clone().unwrap_or_else(|| format!("{:?}", result.status));
        JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": text }] }),
        )
    }
```

Then in `handle_cancel_delegation` at line 795-812, replace:

```rust
            match rx.await {
                Ok(result) => {
                    let text = result.summary.unwrap_or_else(|| match &result.status {
                        DelegationStatus::Failed { error } => error.clone(),
                        other => format!("{:?}", other),
                    });
                    return JsonRpcResponse::success(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    );
                }
                Err(_) => {
                    return JsonRpcResponse::internal_error(
                        id,
                        "cancel_delegation failed: orchestrator disconnected",
                    );
                }
            }
```

with:

```rust
            match rx.await {
                Ok(result) => {
                    return Self::cancel_result_to_response(id, result);
                }
                Err(_) => {
                    return JsonRpcResponse::internal_error(
                        id,
                        "cancel_delegation failed: orchestrator disconnected",
                    );
                }
            }
```

Remove the now-unused `use spur_acp::DelegationStatus;` above that match arm (if it becomes dead). If it was imported at module level, keep it for the new helper.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-mcp cancel_delegation -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run full crate tests**

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "fix(spur-mcp): cancel_delegation stub returns JSON-RPC error, not success (A2)

Previously the handler unwrapped DelegationResult { status: Failed {
error }, summary: None } into a JSON-RPC success whose body text was
the error string. Brain saw \"cancel succeeded\" while the worker kept
running.

Now a Failed result maps to JsonRpcResponse::error(-32601,
\"cancel_delegation: ...\"). Successful results still surface as
success. Real orchestrator-side handler remains pending; until then the
tool honestly reports not-implemented."
```

---

## Task 6: `format_worker_task` helper (T1.2 — TDD)

Pure helper for prepending a `## Relevant Files` section to a worker task string.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add helper + unit tests)

- [ ] **Step 1: Write the failing test**

At the bottom of `crates/spur-core/src/orchestrator.rs` inside a new `#[cfg(test)] mod format_worker_task_tests` block:

```rust
#[cfg(test)]
mod format_worker_task_tests {
    use super::format_worker_task;

    #[test]
    fn empty_list_passes_task_through_unchanged() {
        let task = "Do the thing.";
        let out = format_worker_task(task, &[]);
        assert_eq!(out, task);
    }

    #[test]
    fn single_path_prepends_relevant_files_section() {
        let task = "Do the thing.";
        let files = vec!["src/a.rs".to_string()];
        let out = format_worker_task(task, &files);
        assert!(
            out.starts_with("## Relevant Files\n\n"),
            "expected Relevant Files header first, got: {out}",
        );
        assert!(out.contains("- src/a.rs"));
        assert!(out.contains("## Task\n\nDo the thing."));
    }

    #[test]
    fn multiple_paths_produce_ordered_bullets() {
        let files = vec![
            "crates/spur-mcp/src/server.rs".to_string(),
            "crates/spur-acp/src/adapter/claude.rs".to_string(),
        ];
        let out = format_worker_task("Go.", &files);
        let idx_first = out.find("- crates/spur-mcp/src/server.rs").expect("first bullet");
        let idx_second = out
            .find("- crates/spur-acp/src/adapter/claude.rs")
            .expect("second bullet");
        assert!(idx_first < idx_second, "order must be preserved");
    }

    #[test]
    fn whitespace_task_body_still_gets_section_when_files_nonempty() {
        let out = format_worker_task("   ", &["x.rs".into()]);
        assert!(out.starts_with("## Relevant Files\n\n"));
        assert!(out.trim_end().ends_with("   "));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core format_worker_task -- --nocapture`
Expected: FAIL — `format_worker_task` is undefined.

- [ ] **Step 3: Implement the helper**

Near the top of `crates/spur-core/src/orchestrator.rs` (adjacent to `normalize_agent_name` around line 41), add:

```rust
/// Format a worker task string with an optional `## Relevant Files`
/// section prepended.
///
/// - When `context_files.is_empty()`, the task string is returned
///   unchanged (no section prepended).
/// - Otherwise a `## Relevant Files` header is prepended with each
///   path as a Markdown bullet, followed by a `## Task` header and
///   the original task body. Order of the bullets preserves the input
///   order.
///
/// This function does no file I/O. The worker's own Read tool is
/// responsible for opening the listed paths.
pub(crate) fn format_worker_task(task: &str, context_files: &[String]) -> String {
    if context_files.is_empty() {
        return task.to_string();
    }
    let mut out = String::with_capacity(task.len() + 128 + context_files.len() * 64);
    out.push_str("## Relevant Files\n\n");
    out.push_str(
        "The following files were declared as relevant by the caller. \
         Open them with your Read tool as needed.\n\n",
    );
    for path in context_files {
        out.push_str("- ");
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("\n## Task\n\n");
    out.push_str(task);
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core format_worker_task -- --nocapture`
Expected: PASS (all four cases).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): add format_worker_task helper

Pure helper that prepends a Markdown '## Relevant Files' section to
a worker's task string. Empty list returns task unchanged. Wired into
execute_delegation in a follow-up commit (T1.2 step 2)."
```

---

## Task 7: Wire `context_files` into `execute_delegation` (T1.2)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:2561-2589` (`execute_delegation` signature + top of body)

- [ ] **Step 1: Rename `_context_files` → `context_files` in the signature**

At `crates/spur-core/src/orchestrator.rs:2561-2573`, replace:

```rust
    async fn execute_delegation(
        agent: String,
        original_task: String,
        _context_files: Vec<String>,
        request_id: String,
        brain_session_id: SessionId,
        delegation_plan: Option<spur_acp::domain::DelegationPlan>,
        issue_id: Option<String>,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
    ) -> (DelegationResult, Option<ExecutorId>) {
```

with:

```rust
    async fn execute_delegation(
        agent: String,
        original_task: String,
        context_files: Vec<String>,
        request_id: String,
        brain_session_id: SessionId,
        delegation_plan: Option<spur_acp::domain::DelegationPlan>,
        issue_id: Option<String>,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
    ) -> (DelegationResult, Option<ExecutorId>) {
```

- [ ] **Step 2: Shadow `original_task` with the formatted version**

Immediately after the closing `{` of the function body (line 2574), but BEFORE the `if agent.starts_with("__") { ... }` stub guard, insert:

```rust
        // Shadow `original_task` with the Relevant Files-prepended form
        // so retry loops at orchestrator.rs:3013 reuse the formatted
        // base. No-op when context_files is empty.
        let original_task = format_worker_task(&original_task, &context_files);
```

Result: the first statements inside `execute_delegation` become the shadow, then the `__` guard. Subsequent uses of `original_task` (including the clone at line 2612 and the retry render at 3013) pick up the formatted version automatically.

- [ ] **Step 3: Write an integration-style unit test at the orchestrator layer**

Given the size of `execute_delegation`, a full integration test is costly. Instead, add a focused test at the bottom of `orchestrator.rs`:

```rust
#[cfg(test)]
mod context_files_wiring_tests {
    use super::format_worker_task;

    /// Regression guard: the helper is imported where execute_delegation
    /// lives. If a refactor moves or renames it, the import here breaks
    /// before the wiring silently regresses.
    #[test]
    fn format_worker_task_is_available_in_orchestrator_module() {
        let out = format_worker_task("t", &["x".into()]);
        assert!(out.contains("## Relevant Files"));
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core -- --nocapture`
Expected: PASS (no stale `_context_files` underscore warning; new test passes).

Verify no compiler warnings about unused parameter:
Run: `cargo check -p spur-core 2>&1 | grep -i context_files`
Expected: empty (no unused-variable warnings).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "fix(spur-core): honor context_files in execute_delegation (R1)

The parameter was named _context_files and ignored. Rename to
context_files and shadow original_task with format_worker_task output
at function entry. Retry loop picks up the formatted base because it
reads original_task after the shadow."
```

---

## Task 8: Extend `delegate_parallel` per-task schema with `context_files` (T1.2 + A1)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs:111-148` (`delegate_parallel_def`)
- Modify: `crates/spur-mcp/src/server.rs:564-679` (`handle_delegate_parallel`)

- [ ] **Step 1: Write the failing integration test**

Create `crates/spur-mcp/tests/delegate_parallel_fields.rs`:

```rust
//! Integration tests for delegate_parallel per-task field plumbing
//! (T1.2/A1 + T1.3/R3/A5/A3).
//!
//! These tests exercise handle_delegate_parallel by driving it with a
//! synthetic `DelegationChannel` and asserting each DelegationRequest
//! the server sends carries the right per-task fields.

use serde_json::json;
use spur_mcp::{tools_list, DelegationChannel, DelegationRequest};
use tokio::sync::mpsc;

/// Drain every DelegationRequest the handler sends before its dispatch
/// deadline elapses. Helper used by multiple tests below.
async fn drain_requests(mut rx: mpsc::Receiver<DelegationRequest>, expected: usize) -> Vec<DelegationRequest> {
    let mut out = Vec::with_capacity(expected);
    while out.len() < expected {
        if let Some(req) = rx.recv().await {
            out.push(req);
        } else {
            break;
        }
    }
    out
}

#[tokio::test]
async fn per_task_context_files_survive_to_delegation_requests() {
    // Invoke handle_delegate_parallel with two tasks, each having
    // distinct context_files. Assert each DelegationRequest.context_files
    // reflects its own per-task input.
    //
    // This test is the executable form of RCA A1 + T1.2 commit criterion.
    todo!("wire DelegationChannel + server handle once the public API supports direct handler invocation; see Task 8 step 4");
}
```

(The `todo!()` is intentional and will be replaced in step 4 once the handler wiring is complete.)

- [ ] **Step 2: Verify the integration test file compiles but skips**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields -- --nocapture`
Expected: PASS on compile; the test panics with `not yet implemented` (acceptable — we will flesh it out in step 4 after the handler edit).

- [ ] **Step 3: Extend the per-task schema in `tools.rs`**

In `crates/spur-mcp/src/tools.rs::delegate_parallel_def()` (line 111), replace the `items` block:

```rust
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": { "type": "string", "description": "Worker agent name" },
                            "task":  { "type": "string", "description": "Task description" }
                        },
                        "required": ["agent", "task"]
                    },
                    "description": "List of tasks to delegate in parallel"
                },
```

with:

```rust
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": { "type": "string", "description": "Worker agent name" },
                            "task":  { "type": "string", "description": "Task description" },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional supplementary file paths for this task. Prepended as a '## Relevant Files' section in the worker prompt."
                            }
                        },
                        "required": ["agent", "task"]
                    },
                    "description": "List of tasks to delegate in parallel. Each task carries its own context_files (and, after T1.3, its own issue_id and delegation_plan)."
                },
```

- [ ] **Step 4: Parse per-task `context_files` in the handler**

In `crates/spur-mcp/src/server.rs::handle_delegate_parallel` at line 593-602, immediately after `let task = match task_obj.get("task")...` block, BEFORE `let request_id = uuid::Uuid::new_v4().to_string();`, insert:

```rust
            let context_files: Vec<String> = task_obj
                .get("context_files")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
```

Then at line 611 replace:

```rust
                context_files: Vec::new(),
```

with:

```rust
                context_files,
```

- [ ] **Step 5: Flesh out the integration test**

Replace the `todo!()` body in `crates/spur-mcp/tests/delegate_parallel_fields.rs::per_task_context_files_survive_to_delegation_requests` with:

```rust
    let (tx, rx) = mpsc::channel::<DelegationRequest>(8);
    let channel = DelegationChannel { request_rx: rx };

    // Build the server. `McpCallbackServer::new_for_test` is the public
    // test constructor added in this task if one doesn't already exist;
    // check crates/spur-mcp/src/server.rs for an existing test helper
    // first. If none exists, prefer constructing the handler logic as a
    // free-standing function and testing that instead.
    //
    // If the server type has no test-friendly constructor, the coarse
    // fallback is to call `handle_delegate_parallel` via an integration
    // path — but that requires a full runtime setup. For T1, keep this
    // test at the level of the handler's pure parse logic: call
    // `parse_parallel_tasks(args) -> Vec<DelegationRequest>` (a new
    // free-standing function carved out of the loop body) and assert on
    // its output. See step 4 revision notes.
    //
    // CHOSEN APPROACH: carve a pure helper.

    // This arm is unreachable in the chosen approach; leaving the mpsc
    // setup above in case a later task wires a real server fixture.
    let _ = (tx, channel);

    let args = json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "Task A",
                "context_files": ["src/a1.rs", "src/a2.rs"]
            },
            {
                "agent": "claude-code-acp",
                "task": "Task B",
                "context_files": ["src/b1.rs"]
            }
        ]
    });

    let parsed = spur_mcp::parse_parallel_tasks(&args).expect("parse ok");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].context_files, vec!["src/a1.rs".to_string(), "src/a2.rs".to_string()]);
    assert_eq!(parsed[1].context_files, vec!["src/b1.rs".to_string()]);
}
```

Because the integration test references `spur_mcp::parse_parallel_tasks`, carve out that helper from `handle_delegate_parallel`. In `crates/spur-mcp/src/server.rs`, above `handle_delegate_parallel`, add:

```rust
/// Parse the `tasks` array from a `delegate_parallel` args payload into
/// a list of partially-populated `DelegationRequest` skeletons. Public
/// (crate-level) so integration tests can exercise the parse logic
/// without a live MCP session.
///
/// The returned requests have dummy oneshot senders — do not dispatch
/// them; they are for field-value assertions only.
pub fn parse_parallel_tasks(args: &Value) -> Result<Vec<DelegationRequest>, String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut out = Vec::with_capacity(tasks.len());
    for task_obj in tasks {
        let agent = task_obj
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "task.agent missing".to_string())?
            .to_string();
        let task = task_obj
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "task.task missing".to_string())?
            .to_string();
        let context_files: Vec<String> = task_obj
            .get("context_files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        out.push(DelegationRequest {
            id: uuid::Uuid::new_v4().to_string(),
            agent,
            task,
            context_files,
            respond_to: tx,
            brain_session_id: spur_acp::SessionId::new(uuid::Uuid::new_v4().to_string()),
            delegation_plan: None,
            issue_id: None,
        });
    }
    Ok(out)
}
```

Update `handle_delegate_parallel` (line 582-637) to call `parse_parallel_tasks` for parsing, then attach the real oneshot channels before dispatch. Replace the existing loop body (the stretch from `for task_obj in &tasks { ... receivers.push((request_id, agent)); }`) with:

```rust
        let skeletons = match parse_parallel_tasks(&args) {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        for mut skeleton in skeletons {
            let request_id = skeleton.id.clone();
            let agent = skeleton.agent.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            skeleton.respond_to = tx;
            skeleton.brain_session_id = self.brain_session_id.clone();

            if let Err(_e) = self.delegation_tx.send(skeleton).await {
                error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.active_delegations
                .lock()
                .await
                .insert(request_id.clone());
            Self::spawn_result_collector(
                &self.task_tracker,
                request_id.clone(),
                rx,
                Arc::clone(&self.active_delegations),
                Arc::clone(&self.completed_delegations),
            );
            receivers.push((request_id, agent));
        }
```

Note: the `shared_plan` and `shared_issue_id` parse statements at lines 574-580 remain in this task for now (removed in Task 10). They are simply no longer read inside the loop.

Export `parse_parallel_tasks` from the crate. In `crates/spur-mcp/src/lib.rs:8`:

```rust
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};
```

extend to:

```rust
pub use server::{build_worker_info, parse_parallel_tasks, McpCallbackServer, WorkerInfo};
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};
```

- [ ] **Step 6: Run the integration test**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields per_task_context_files`
Expected: PASS.

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/src/lib.rs crates/spur-mcp/tests/delegate_parallel_fields.rs
git commit -m "feat(spur-mcp): delegate_parallel per-task context_files (A1)

Adds context_files to the per-task schema and parses it in
handle_delegate_parallel. Each parallel worker now receives its own
declared scope rather than an empty vector. Closes A1 from the T1
spec. parse_parallel_tasks extracted for integration-test access.

T1.3 (per-task issue_id + delegation_plan) and top-level removal
follow in the next commits."
```

---

## Task 9: Extend per-task schema with `issue_id` + `delegation_plan`; drop top-level `issue_id` (T1.3)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (`delegate_parallel_def`)
- Modify: `crates/spur-mcp/src/server.rs` (`handle_delegate_parallel`, `parse_parallel_tasks`)
- Modify: `crates/spur-mcp/tests/delegate_parallel_fields.rs`

- [ ] **Step 1: Add failing test for per-task issue_id + delegation_plan**

Append to `crates/spur-mcp/tests/delegate_parallel_fields.rs`:

```rust
#[test]
fn per_task_issue_id_and_delegation_plan_survive_unshared() {
    let args = json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "Task A",
                "issue_id": "bd-1",
                "delegation_plan": { "chosen": "claude-code-acp", "rationale": "A rationale" }
            },
            {
                "agent": "gpt-5-acp",
                "task": "Task B",
                "issue_id": "bd-2",
                "delegation_plan": { "chosen": "gpt-5-acp", "rationale": "B rationale" }
            }
        ],
        "delegation_plan": { "chosen": "batch-top-level", "rationale": "SHOULD NOT propagate" }
    });

    let parsed = spur_mcp::parse_parallel_tasks(&args).expect("parse ok");
    assert_eq!(parsed.len(), 2);

    assert_eq!(parsed[0].issue_id.as_deref(), Some("bd-1"));
    assert_eq!(parsed[1].issue_id.as_deref(), Some("bd-2"));

    // Per-task plans are distinct.
    let p0 = parsed[0].delegation_plan.as_ref().expect("plan A present");
    let p1 = parsed[1].delegation_plan.as_ref().expect("plan B present");
    assert_eq!(p0.chosen.as_deref(), Some("claude-code-acp"));
    assert_eq!(p1.chosen.as_deref(), Some("gpt-5-acp"));

    // Top-level plan from the args MUST NOT have been propagated.
    assert!(
        p0.chosen.as_deref() != Some("batch-top-level"),
        "top-level delegation_plan leaked into per-task request",
    );
    assert!(p1.chosen.as_deref() != Some("batch-top-level"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields per_task_issue_id_and_delegation_plan -- --nocapture`
Expected: FAIL — `parse_parallel_tasks` does not yet parse `issue_id` or `delegation_plan` per task.

- [ ] **Step 3: Update `delegate_parallel_def` schema**

In `crates/spur-mcp/src/tools.rs::delegate_parallel_def()`, replace the `items.properties` block extended in Task 8:

```rust
                        "properties": {
                            "agent": { "type": "string", "description": "Worker agent name" },
                            "task":  { "type": "string", "description": "Task description" },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional supplementary file paths for this task. Prepended as a '## Relevant Files' section in the worker prompt."
                            }
                        },
```

with:

```rust
                        "properties": {
                            "agent": { "type": "string", "description": "Worker agent name" },
                            "task":  { "type": "string", "description": "Task description" },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional supplementary file paths for this task. Prepended as a '## Relevant Files' section in the worker prompt."
                            },
                            "issue_id": {
                                "type": "string",
                                "description": "Optional beads issue ID to auto-track for this task. Must be unique across tasks in a single batch."
                            },
                            "delegation_plan": {
                                "type": "object",
                                "description": "Per-task structured reasoning. Used for reviewer mismatch detection. Takes precedence over the batch-level delegation_plan."
                            }
                        },
```

Also remove the top-level `issue_id` property from `delegate_parallel_def`. Delete the block:

```rust
                "issue_id": {
                    "type": "string",
                    "description": "Optional beads issue ID to auto-track"
                }
```

and update the top-level `delegation_plan` description:

```rust
                "delegation_plan": {
                    "type": "object",
                    "description": "Batch-level decomposition rationale. Documents why these N subtasks together and how they are independent. Per-task delegation_plan (inside tasks[]) takes precedence for reviewer mismatch checks.",
                    "properties": {
                        "candidates":    { "type": "array" },
                        "decomposition": { "type": "array" },
                        "chosen":        { "type": "string" },
                        "rationale":     { "type": "string" }
                    }
                },
```

- [ ] **Step 4: Extend `parse_parallel_tasks` to read per-task issue_id + delegation_plan**

In `crates/spur-mcp/src/server.rs::parse_parallel_tasks`, update the loop body after the `context_files` parse, before the `push`:

```rust
        let issue_id = task_obj
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let delegation_plan: Option<spur_acp::DelegationPlan> = task_obj
            .get("delegation_plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
```

Then in the `push`:

```rust
        out.push(DelegationRequest {
            id: uuid::Uuid::new_v4().to_string(),
            agent,
            task,
            context_files,
            respond_to: tx,
            brain_session_id: spur_acp::SessionId::new(uuid::Uuid::new_v4().to_string()),
            delegation_plan,
            issue_id,
        });
```

- [ ] **Step 5: Remove shared-plan and shared-issue-id parses + clones from `handle_delegate_parallel`**

At lines 574-580 of the handler, delete:

```rust
        let shared_plan: Option<spur_acp::DelegationPlan> = args
            .get("delegation_plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let shared_issue_id = args
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(String::from);
```

Replace with a single info-log of the top-level plan (non-propagated audit, per spec §4.3):

```rust
        if let Some(batch_plan) = args.get("delegation_plan") {
            tracing::info!(
                batch_plan = %batch_plan,
                "delegate_parallel received batch-level delegation_plan (not propagated into per-task requests)",
            );
        }
```

The existing handler body (after Task 8's carve-out) no longer references `shared_plan` or `shared_issue_id`. Remove any remaining references to them.

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields -- --nocapture`
Expected: PASS (both per-task tests now green).

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/delegate_parallel_fields.rs
git commit -m "feat(spur-mcp): delegate_parallel per-task issue_id + delegation_plan (R3/A3/A5)

Per-task schema gains issue_id (optional) and delegation_plan
(optional). Top-level issue_id removed (no coherent batch meaning).
Top-level delegation_plan retained as batch decomposition rationale but
no longer cloned into per-task DelegationRequest — it is logged once
per call for audit and discarded.

Closes RCA R3/A5 (shared identity race) and A3 (tautological reviewer
mismatch check) from the T1 spec."
```

---

## Task 10: Per-task `issue_id` uniqueness validation (T1.3 HX2)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:handle_delegate_parallel`
- Modify: `crates/spur-mcp/tests/delegate_parallel_fields.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-mcp/tests/delegate_parallel_fields.rs`:

```rust
#[test]
fn duplicate_non_none_issue_id_is_rejected() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A", "issue_id": "bd-1" },
            { "agent": "x", "task": "B", "issue_id": "bd-1" }
        ]
    });
    let err = spur_mcp::validate_parallel_args(&args)
        .expect_err("duplicate issue_id must be rejected");
    assert!(
        err.contains("issue_id"),
        "error should mention issue_id: {err}",
    );
}

#[test]
fn duplicate_none_issue_id_across_tasks_is_allowed() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A" },
            { "agent": "x", "task": "B" }
        ]
    });
    spur_mcp::validate_parallel_args(&args).expect("None-id twice is fine");
}

#[test]
fn distinct_issue_ids_pass() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A", "issue_id": "bd-1" },
            { "agent": "x", "task": "B", "issue_id": "bd-2" }
        ]
    });
    spur_mcp::validate_parallel_args(&args).expect("distinct ids pass");
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields duplicate -- --nocapture`
Expected: FAIL — `validate_parallel_args` is undefined.

- [ ] **Step 3: Implement `validate_parallel_args`**

In `crates/spur-mcp/src/server.rs`, alongside `parse_parallel_tasks`, add:

```rust
/// Validate args for `delegate_parallel` beyond what the schema shape
/// enforces. Currently: per-task `issue_id` values must be pairwise
/// unique across the batch when non-null. Public (crate-level) for
/// integration test access.
pub fn validate_parallel_args(args: &Value) -> Result<(), String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (idx, task) in tasks.iter().enumerate() {
        if let Some(id) = task.get("issue_id").and_then(|v| v.as_str()) {
            if !seen.insert(id) {
                return Err(format!(
                    "delegate_parallel: issue_id values must be unique across tasks (duplicate '{id}' at index {idx})",
                ));
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire into `handle_delegate_parallel` before dispatch**

At the top of `handle_delegate_parallel`, after the `tasks.is_empty()` check, insert:

```rust
        if let Err(e) = validate_parallel_args(&args) {
            return JsonRpcResponse::invalid_params(id, e);
        }
```

- [ ] **Step 5: Export the function**

In `crates/spur-mcp/src/lib.rs`, extend the `pub use` line from Task 8:

```rust
pub use server::{build_worker_info, parse_parallel_tasks, validate_parallel_args, McpCallbackServer, WorkerInfo};
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-mcp --test delegate_parallel_fields -- --nocapture`
Expected: PASS (all uniqueness cases green).

Run: `cargo test -p spur-mcp`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/lib.rs crates/spur-mcp/tests/delegate_parallel_fields.rs
git commit -m "feat(spur-mcp): reject duplicate per-task issue_id in delegate_parallel (HX2)

Closes INV-3 from the T1 spec. If the caller accidentally assigns the
same non-null issue_id to two parallel tasks, the handler rejects the
call with JsonRpcResponse::invalid_params before any DelegationRequest
is dispatched. None/None collisions across tasks are allowed because
None means 'do not track an issue for this worker'."
```

---

## Task 11: Clean up dead `__progress` / `__session_cost` branches in orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:2574-2589` (`execute_delegation` stub guard)

- [ ] **Step 1: Read the current stub-guard**

The current block at `crates/spur-core/src/orchestrator.rs:2574-2589`:

```rust
        // Internal operations (progress, cost) — still stubbed.
        if agent.starts_with("__") {
            return (
                DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Internal operation not yet wired: {}", agent),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                },
                None,
            );
        }
```

- [ ] **Step 2: Tighten the guard to only `__cancel_delegation`**

Replace with:

```rust
        // Internal operation: __cancel_delegation. Still stubbed until a
        // real orchestrator-side cancellation handler lands. Any other
        // `__`-prefixed agent name is an error (no longer reachable from
        // the MCP server — report_progress and get_session_cost were
        // removed in T1).
        if agent.starts_with("__") {
            let error = if agent == "__cancel_delegation" {
                "Internal operation not yet wired: __cancel_delegation".to_string()
            } else {
                format!("Unsupported internal operation: {agent}")
            };
            return (
                DelegationResult {
                    status: DelegationStatus::Failed { error },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                },
                None,
            );
        }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core`
Expected: PASS.

Run: `cargo test -p spur-mcp`
Expected: PASS (cancel_delegation stub still returns the expected error; Task 5's test covers it).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "refactor(spur-core): narrow __-agent stub guard to __cancel_delegation

After T1 removed report_progress and get_session_cost from the MCP
surface, __progress and __session_cost are unreachable. The guard now
treats __cancel_delegation explicitly and reports other __-prefixed
agent names as 'unsupported' rather than 'not yet wired'."
```

---

## Task 12: Brain prompt audit

Verify no shipped brain system prompt statically references removed tools or the removed top-level `delegate_parallel.issue_id` field.

**Files:**
- Audit: `crates/spur-acp/src/agents/defaults.rs` and any other shipped prompt templates

- [ ] **Step 1: Grep for references**

Run each of these:

```bash
grep -rn "report_progress" crates/spur-acp/src/agents/ || echo "clean"
grep -rn "get_session_cost" crates/spur-acp/src/agents/ || echo "clean"
grep -rn 'delegate_parallel.*issue_id' crates/ --include="*.rs" --include="*.md" || echo "clean"
```

Record the output.

- [ ] **Step 2: Update any static references found**

For each hit in a shipped (non-test, non-doc, non-event-log) file:
- If the reference is a factual description of a removed tool, delete or rewrite.
- If the reference tells the brain how to use a removed tool, delete the paragraph.
- If the reference is in `defaults.rs`, update the brain's tool guide to reflect the post-T1 catalog.

If all grep runs return "clean", no changes needed.

- [ ] **Step 3: Run full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit (only if edits made)**

```bash
git add <changed files>
git commit -m "docs(spur-acp): remove references to report_progress / get_session_cost

Part of the T1 contract-truthfulness cleanup. No runtime behavior
change — brain system prompts updated to reflect the post-T1 tool
catalog."
```

If no edits were made, skip this commit and note "no brain prompt references found" in the PR description.

---

## Task 13: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Run clippy with warnings-as-errors for the two changed crates**

Run:

```bash
cargo clippy -p spur-mcp --all-targets -- -D warnings
cargo clippy -p spur-core --all-targets -- -D warnings
```

Expected: PASS both.

- [ ] **Step 2: Run the full test suite**

Run:

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 3: Confirm INV-1 catalog test matches expectations**

Run: `cargo test -p spur-mcp --test tool_catalog`
Expected: PASS — EXPECTED list no longer contains `report_progress` or `get_session_cost`.

- [ ] **Step 4: Confirm integration tests cover all commit criteria from spec §4**

Cross-check the spec's commit criteria list:

- spec T1.1 `handle_cancel_delegation` returns JSON-RPC error on Failed stub → covered by Task 5's unit tests.
- spec T1.1 `report_progress`, `get_session_cost` removed → covered by Task 1's snapshot test after Tasks 3 + 4.
- spec T1.2 `execute_delegation` consumes `context_files` → covered by Task 7 (underscore removed → compiler-enforced) + Task 6 helper tests.
- spec T1.2 `delegate_parallel` per-task `context_files` → covered by Task 8's integration test.
- spec T1.3 per-task fields unshared + top-level plan not propagated → covered by Task 9's integration test.
- spec T1.3 HX2 uniqueness → covered by Task 10's three unit tests.
- spec T1.4 `source` pruned → covered by Task 2's schema tests.
- spec INV-4 (per-task plan never equals top-level) → covered by Task 9 (explicit assertion).

Record the pass/fail state in the PR description.

- [ ] **Step 5: No commit for this task**

Verification only. If any step failed, backfill the missing test or fix and commit under the appropriate task number.

---

## Task 14: Commit the spec document

**Files:** `docs/superpowers/specs/2026-04-18-mcp-contract-truthfulness-design.md`

The spec file already exists uncommitted. Include it alongside the T1 work so the design record lives in git.

- [ ] **Step 1: Stage the spec and RCA phase-2 update**

```bash
git add docs/superpowers/specs/2026-04-18-mcp-contract-truthfulness-design.md
git add docs/rca/2026-04-18-brain-worker-beads-mcp-journey.md
```

- [ ] **Step 2: Commit**

```bash
git commit -m "docs(spur): T1 contract-truthfulness spec + RCA phase-2 addendum

Source design spec for the MCP contract-truthfulness work (T1 of the
T1 → T3 → T2 convergence roadmap from the brain-worker RCA). Addendum
on the RCA records MCTS grounding results, adversarial findings
(A1-A4), sequence diagrams, and tiered remediation ordering."
```

---

## Self-review

I reviewed this plan against the spec. Results:

**Spec coverage:**
- §4.1 (T1.1 control-plane) → Tasks 3, 4, 5, 11. ✓
- §4.2 (T1.2 context_files) → Tasks 6, 7, 8. ✓
- §4.3 (T1.3 parallel fields) → Tasks 9, 10. ✓
- §4.4 (T1.4 source removal) → Task 2. ✓
- §6 INV-1 → Task 1 (catalog snapshot); deferred full schema-handler parity declared in §8.4. ✓
- §6 INV-2 (context_files structurally consumed) → compiler-enforced by underscore removal + Task 7 test. ✓
- §6 INV-3 (per-task issue_id unique) → Task 10. ✓
- §6 INV-4 (per-task plan never equals top-level) → Task 9. ✓
- §6 INV-5 (cancel Failed → error) → Task 5. ✓
- §7.5 (brain prompt audit) → Task 12. ✓
- §8 (testing) → Tasks 2, 5, 6, 8, 9, 10 all carry tests. ✓

**Placeholder scan:**
- No TBD / TODO / "handle edge cases" / "similar to Task N" / "add appropriate error handling" patterns.
- Task 8 Step 5 does contain a `todo!()` in its *initial* integration-test scaffold — but the same step replaces it with real code later in the same step. The engineer runs Step 2 (compile-only) between, which is why the `todo!()` appears transiently.

**Type consistency:**
- `format_worker_task(&str, &[String]) -> String` — same signature in Task 6 definition and Task 7 usage.
- `parse_parallel_tasks(&Value) -> Result<Vec<DelegationRequest>, String>` — defined in Task 8, referenced by Tasks 9, 10.
- `validate_parallel_args(&Value) -> Result<(), String>` — defined and used in Task 10 only.
- `cancel_result_to_response(Value, DelegationResult) -> JsonRpcResponse` — defined Task 5, not used elsewhere.
- Tool names (`delegate_parallel`, etc.) consistent across tasks and snapshot test.

No fixes required.

---

## Out-of-scope / parked items (for future specs)

- Real `__cancel_delegation` orchestrator-side handler — requires JoinHandle tracking per request, separate spec.
- Real `report_progress` wiring as `SpurEventBody::BrainProgress` emitter — separate spec.
- Multi-backend `source` routing — requires `PmService` multi-instance plumbing.
- T2 Beads atomicity (R4) and success-IssueUpdated (A4).
- T3 hierarchy split (R5) and poll parity (R6).
- Full schema-handler parity test via static analysis or typed structs — §8.4 left both options open; Task 1's catalog snapshot is the minimal guard shipped in T1.

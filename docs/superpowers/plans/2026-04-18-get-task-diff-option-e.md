# `get_task_diff` — Option E Implementation Plan

> **For agentic workers:** Implement via superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Fix `get_task_diff` to return the full task delta even when the worker self-commits before exiting, and always emit a structured response key when no diff is available (never silently omit).

**Architecture:** Two surgical changes in Rust. (1) Collection: `collect_diff` falls back to `git diff <base_commit>..HEAD` when `git diff HEAD` is empty (worker already committed); `build_diff_summary` uses the same basis. (2) Handler: `handle_get_task_diff` always inserts the `"diff"` key — when the value is null it also inserts `"diff_status": "no_changes_detected"` and `"diff_basis"`.

**Tech Stack:** Rust 2021, tokio, serde, git CLI via `tokio::process::Command`.

**Spec:** `docs/rca/2026-04-18-get-task-diff-empty.md` (Option E recommendation).

---

## File map

| File | Task | Change |
|---|---|---|
| `crates/spur-worktree/src/manager.rs` | 1 | `collect_diff` returns `(Option<String>, &'static str)` — text + basis used |
| `crates/spur-core/src/orchestrator.rs` | 1 | Pass basis from `collect_diff` into `build_diff_summary`; `build_diff_summary` takes `basis: &str` arg |
| `crates/spur-mcp/src/server.rs` | 2 | `handle_get_task_diff` always emits `"diff"` key with marker on null |

Total: ~35 LOC + 4 tests.

**Explicit non-goals (from RCA):**
- No `commit_sha` field on `DelegationResult` (rejected Option B).
- No `DelegationResult` schema change.
- No `PlanState` durability.
- No retry/backoff on `collect_diff`.

---

## Task 1: Collection fix — base-aware fallback + parallel summary basis

**Goal:** `collect_diff` captures the task delta even when the worker self-committed; `build_diff_summary` uses the same basis.

### Step 1.1: Read current shape

- [ ] Read `crates/spur-worktree/src/manager.rs` lines 180-220 to see `WorktreeInfo` (includes `base_commit: String` field) and the current `collect_diff` function.
- [ ] Read `crates/spur-core/src/orchestrator.rs` lines 3680-3720 (callsite) and 3815-3870 (`build_diff_summary`).

### Step 1.2: Write the failing test for the fallback behaviour

In `crates/spur-worktree/src/manager.rs` — find the existing `#[cfg(test)] mod tests` block if any, otherwise create one at the bottom of the file. Append:

```rust
#[cfg(test)]
mod tests_option_e {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Run a sequence of git commands in a dir. Panics on first error —
    /// test scaffolding only.
    async fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .expect("git command failed to spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Build a minimal repo with one "base" commit, return (tempdir, base_sha).
    async fn seed_base_repo(tmp: &std::path::Path) -> String {
        git(tmp, &["init", "-q", "-b", "main"]).await;
        git(tmp, &["config", "user.email", "test@example.com"]).await;
        git(tmp, &["config", "user.name", "Test"]).await;
        tokio::fs::write(tmp.join("a.txt"), "base\n").await.unwrap();
        git(tmp, &["add", "a.txt"]).await;
        git(tmp, &["commit", "-q", "-m", "base"]).await;
        git(tmp, &["rev-parse", "HEAD"]).await
    }

    #[tokio::test]
    async fn collect_diff_falls_back_to_base_when_head_empty() {
        let tmp = TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // Worker commits their change (the scenario that broke bd-1mh.2).
        tokio::fs::write(tmp.path().join("a.txt"), "worker change\n").await.unwrap();
        git(tmp.path(), &["add", "a.txt"]).await;
        git(tmp.path(), &["commit", "-q", "-m", "worker commit"]).await;

        // Working tree is clean; `git diff HEAD` is empty.
        // Fallback to base_commit..HEAD should capture the worker's commit.
        let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        let sid = agent_client_protocol::SessionId::new(Arc::<str>::from("s1"));
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        let diff = diff.expect("expected Some(diff) via fallback, got None");
        assert!(diff.contains("worker change"), "diff should contain worker's change, got: {diff}");
        assert_eq!(basis, "base_commit..HEAD");
    }

    #[tokio::test]
    async fn collect_diff_returns_head_basis_when_uncommitted_changes_exist() {
        let tmp = TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // Worker leaves uncommitted changes (NOT the self-commit scenario).
        tokio::fs::write(tmp.path().join("a.txt"), "uncommitted\n").await.unwrap();

        let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        let sid = agent_client_protocol::SessionId::new(Arc::<str>::from("s2"));
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        let diff = diff.expect("expected Some(diff) from HEAD path");
        assert!(diff.contains("uncommitted"), "diff should capture uncommitted, got: {diff}");
        assert_eq!(basis, "HEAD");
    }

    #[tokio::test]
    async fn collect_diff_returns_none_when_no_changes() {
        let tmp = TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // No changes.
        let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        let sid = agent_client_protocol::SessionId::new(Arc::<str>::from("s3"));
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        assert!(diff.is_none(), "expected None for no-change scenario");
        // Basis is still the attempted fallback.
        assert_eq!(basis, "base_commit..HEAD");
    }
}
```

**Note:** `WorktreeManager::new_for_test` and `register_for_test` are test-only helpers you'll need to add. They're minimal:

```rust
#[cfg(test)]
impl WorktreeManager {
    pub fn new_for_test(repo_root: std::path::PathBuf) -> Self {
        Self {
            repo_root,
            active: std::collections::HashMap::new(),
        }
    }

    pub fn register_for_test(
        &self,
        session_id: agent_client_protocol::SessionId,
        path: std::path::PathBuf,
        branch: String,
        base_commit: String,
        agent: String,
    ) {
        // `active` is likely behind a Mutex/DashMap — use the same pattern
        // the rest of the file uses. If `active` is `HashMap<String, WorktreeInfo>`
        // without a lock, use interior mutability or a Mutex. Inspect the file
        // structure first.
    }
}
```

If `WorktreeManager.active` is NOT directly mutable from a `&self` method (e.g., it's a `HashMap` without interior mutability), either:
- (a) Change `register_for_test` to take `&mut self` AND update the test scaffolding accordingly.
- (b) Add a `Mutex<HashMap<...>>` if that's the actual structure.

Inspect the struct definition around line 180 before writing the helper.

### Step 1.3: Run tests — verify FAIL

Run: `cargo test -p spur-worktree --lib collect_diff_`
Expected: FAIL — `collect_diff` currently returns `Result<Option<String>>`, the test expects `Result<(Option<String>, &'static str)>`.

### Step 1.4: Change `collect_diff` signature + add fallback

Edit `crates/spur-worktree/src/manager.rs`. Replace the existing `collect_diff`:

```rust
/// Collect the diff of the worker's task. Returns:
/// - `(Some(diff), "HEAD")` if the worker left uncommitted changes.
/// - `(Some(diff), "base_commit..HEAD")` if the worker already committed
///   (HEAD-relative diff is empty, but base..HEAD has content).
/// - `(None, "base_commit..HEAD")` if the worker produced no changes at
///   all. Caller distinguishes "no changes" from "collection failed" via
///   the returned basis.
pub async fn collect_diff(
    &self,
    session_id: &SessionId,
) -> Result<(Option<String>, &'static str)> {
    let info = self.lookup(session_id)?;

    // First: HEAD-relative (uncommitted changes).
    let head_diff = self
        .run_git(&["diff", "HEAD"], Some(&info.path))
        .await
        .context("failed to collect HEAD-relative diff")?;

    if !head_diff.is_empty() {
        return Ok((Some(head_diff), "HEAD"));
    }

    // Fallback: base_commit..HEAD (worker self-committed).
    let base_spec = format!("{}..HEAD", info.base_commit);
    let base_diff = self
        .run_git(&["diff", &base_spec], Some(&info.path))
        .await
        .context("failed to collect base..HEAD diff")?;

    if base_diff.is_empty() {
        Ok((None, "base_commit..HEAD"))
    } else {
        Ok((Some(base_diff), "base_commit..HEAD"))
    }
}
```

### Step 1.5: Run test again — verify PASS

Run: `cargo test -p spur-worktree --lib collect_diff_`
Expected: 3 tests PASS.

### Step 1.6: Update `build_diff_summary` to accept a basis

Edit `crates/spur-core/src/orchestrator.rs:3822`. Change signature:

```rust
/// Compute a `DiffSummary` for a worktree via `git diff --numstat <basis>`.
///
/// `basis` must match what `collect_diff` used for the raw diff — either
/// "HEAD" or "<base_commit>..HEAD" (rendered with the actual SHA). Otherwise
/// the raw diff text and the structured summary disagree.
async fn build_diff_summary(
    worktree_path: &std::path::Path,
    basis: &str,
) -> anyhow::Result<spur_acp::DiffSummary> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg(basis)
        .current_dir(worktree_path)
        .output()
        .await?;
    // ... (rest of function unchanged)
```

The function body from `if !output.status.success()` downward is identical — only the argument to `.arg()` changes from `"HEAD"` (literal) to `basis` (the passed-in string).

### Step 1.7: Update the callsite in orchestrator.rs:3687

Edit `crates/spur-core/src/orchestrator.rs:3687-3711`. The old block:

```rust
// 4. Collect diff.
let diff = worktrees
    .collect_diff(&worker_session)
    .await
    .unwrap_or(None);

// ...

let diff_summary = if diff.is_some() {
    build_diff_summary(&worktree_path)
        .await
        .ok()
        .filter(|s| s.files_changed > 0)
} else {
    None
};
```

Becomes:

```rust
// 4. Collect diff. `basis` is either "HEAD" (uncommitted) or
// "<base>..HEAD" (worker self-committed). We need it to compute the
// matching diff_summary with the SAME git range — otherwise stats and
// raw text disagree.
let (diff, diff_basis) = worktrees
    .collect_diff(&worker_session)
    .await
    .unwrap_or((None, "HEAD"));

// ...

// Compute structured diff stats on the SAME basis as the raw diff.
// When collect_diff returned base..HEAD, we need to resolve the placeholder
// to the real spec — fetch the base_commit from worktrees.
let diff_summary = if diff.is_some() {
    let basis_spec = if diff_basis == "base_commit..HEAD" {
        // Resolve the placeholder with the actual base SHA.
        worktrees
            .active
            .get(&worker_session.to_string())
            .map(|i| format!("{}..HEAD", i.base_commit))
            .unwrap_or_else(|| "HEAD".to_string())
    } else {
        "HEAD".to_string()
    };
    build_diff_summary(&worktree_path, &basis_spec)
        .await
        .ok()
        .filter(|s| s.files_changed > 0)
} else {
    None
};
```

### Step 1.8: Build

Run: `cargo build -p spur-worktree -p spur-core`
Expected: builds clean. If not — a common issue: the returned `&'static str` from `collect_diff` needs to outlive the task. Since they're literals, this works.

If `worktrees.active.get(...)` doesn't compile (e.g., active is behind a lock), use the same pattern already in the file (line 3695).

### Step 1.9: Run full test suite

Run: `cargo test -p spur-worktree --lib`
Run: `cargo test -p spur-core --lib`
Expected: all PASS.

### Step 1.10: Commit

```bash
git add crates/spur-worktree/src/manager.rs crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
feat(worktree): collect_diff falls back to base_commit..HEAD

When `git diff HEAD` is empty (worker self-committed before exit),
collect_diff falls back to `git diff <base_commit>..HEAD` to capture
the full task delta. Returns (Option<String>, &'static str) tuple so
callers know which basis was used; build_diff_summary consumes the
same basis, so raw diff and structured stats always agree.

Fixes the bd-1mh.2 empty-diff regression (see RCA
docs/rca/2026-04-18-get-task-diff-empty.md).

Implements T1 of get_task_diff-option-e.
EOF
)"
```

---

## Task 2: Handler marker — always emit `diff` key

**Goal:** `handle_get_task_diff` never silently omits the `"diff"` key. When null, it also emits `"diff_status"` and `"diff_basis"` so the brain can distinguish "correct no-change outcome" from "collection failed."

### Step 2.1: Write the failing test

Find the existing tests in `crates/spur-mcp/src/server.rs` (if any) or create a `#[cfg(test)] mod tests` block. Because `McpCallbackServer` has many dependencies that make in-module testing tricky, this test goes in `plan.rs` instead (where other plan-related tests live), testing the JSON shape contract the handler guarantees. Append to `crates/spur-mcp/src/plan.rs` `mod tests`:

Actually — the handler logic is in `server.rs`, not `plan.rs`. The right place for this test is a dedicated integration test in `crates/spur-mcp/tests/`. But the existing test style in this repo is in-module unit tests. Take the pragmatic path: extract the response-building logic into a pure function in `server.rs` or `plan.rs`, and test that.

**Extract-and-test approach:** add to `crates/spur-mcp/src/plan.rs` (near the other helpers around line 920):

```rust
/// Build the JSON fields for a `get_task_diff` response given a
/// DelegationResult. Pure — no I/O. Owns the contract that the "diff"
/// key is ALWAYS present when the task has a result; when the diff is
/// None, a structured marker tells the brain why.
pub(crate) fn build_task_diff_fields(
    result: &spur_acp::DelegationResult,
) -> Vec<(String, serde_json::Value)> {
    use serde_json::json;
    let mut out: Vec<(String, serde_json::Value)> = Vec::new();

    match &result.diff {
        Some(diff) => {
            out.push(("diff".into(), json!(diff)));
        }
        None => {
            out.push(("diff".into(), serde_json::Value::Null));
            out.push(("diff_status".into(), json!("no_changes_detected")));
            out.push(("diff_basis".into(), json!("base_commit..HEAD")));
        }
    }
    if let Some(ref ds) = result.diff_summary {
        out.push(("diff_summary".into(), serde_json::to_value(ds).unwrap_or_default()));
    }
    if let Some(ref s) = result.summary {
        out.push(("summary".into(), json!(s)));
    }
    out
}
```

Then append to `mod tests` in `plan.rs`:

```rust
#[test]
fn build_task_diff_fields_emits_marker_when_diff_none() {
    let result = spur_acp::DelegationResult {
        diff: None,
        diff_summary: None,
        summary: Some("did work".to_string()),
        // ... include other required fields with defaults; if
        // DelegationResult has many fields, use Default::default() if
        // implemented, else construct minimally.
        ..Default::default()
    };
    let fields = super::build_task_diff_fields(&result);
    let m: std::collections::HashMap<String, serde_json::Value> =
        fields.into_iter().collect();

    // diff key ALWAYS present.
    assert!(m.contains_key("diff"), "diff key must always be present");
    assert!(m["diff"].is_null(), "diff value should be null, got {:?}", m["diff"]);

    // Marker fields explain the null.
    assert_eq!(m.get("diff_status").and_then(|v| v.as_str()), Some("no_changes_detected"));
    assert_eq!(m.get("diff_basis").and_then(|v| v.as_str()), Some("base_commit..HEAD"));

    // Existing summary field still present.
    assert_eq!(m.get("summary").and_then(|v| v.as_str()), Some("did work"));
}

#[test]
fn build_task_diff_fields_emits_diff_when_present() {
    let result = spur_acp::DelegationResult {
        diff: Some("diff --git a/x b/x\n...".to_string()),
        diff_summary: None,
        summary: None,
        ..Default::default()
    };
    let fields = super::build_task_diff_fields(&result);
    let m: std::collections::HashMap<String, serde_json::Value> =
        fields.into_iter().collect();

    assert_eq!(m.get("diff").and_then(|v| v.as_str()), Some("diff --git a/x b/x\n..."));
    assert!(!m.contains_key("diff_status"), "diff_status only when diff is null");
    assert!(!m.contains_key("diff_basis"), "diff_basis only when diff is null");
}
```

If `DelegationResult` doesn't impl `Default`, construct it explicitly with all required fields. Inspect `crates/spur-acp/src/domain/delegation.rs` (or wherever `DelegationResult` lives) to see the shape.

### Step 2.2: Run test — verify FAIL

Run: `cargo test -p spur-mcp --lib build_task_diff_fields`
Expected: FAIL with `cannot find function build_task_diff_fields`.

### Step 2.3: Add the pure helper (shown in Step 2.1)

### Step 2.4: Run test — verify PASS

Run: `cargo test -p spur-mcp --lib build_task_diff_fields`
Expected: 2 tests PASS.

### Step 2.5: Wire the helper into `handle_get_task_diff`

Edit `crates/spur-mcp/src/server.rs:1638-1648`. Replace:

```rust
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
```

with:

```rust
if let Some(ref result) = entry.result {
    for (k, v) in crate::plan::build_task_diff_fields(result) {
        resp.insert(k, v);
    }
}
```

### Step 2.6: Build + run full test suite

Run: `cargo build -p spur-mcp`
Run: `cargo test -p spur-mcp --lib`
Expected: all PASS (including the two new `build_task_diff_fields_*` tests and all existing tests).

### Step 2.7: Commit

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/src/server.rs
git commit -m "$(cat <<'EOF'
feat(mcp): handle_get_task_diff always emits "diff" key with marker

When result.diff is None, the handler now inserts the "diff" key with
value `null` plus "diff_status": "no_changes_detected" and "diff_basis":
"base_commit..HEAD". The brain can distinguish a correct no-change
outcome (approve on summary) from a collection failure (escalate).

Response-building logic extracted into pure build_task_diff_fields for
testability.

Implements T2 of get_task_diff-option-e.
EOF
)"
```

---

## Final verification

### Step F.1: Full workspace build + test

Run: `cargo build --workspace`
Run: `cargo test --workspace --lib`
Expected: clean build, all tests PASS. New tests:
- `spur-worktree::manager::tests_option_e::collect_diff_falls_back_to_base_when_head_empty`
- `spur-worktree::manager::tests_option_e::collect_diff_returns_head_basis_when_uncommitted_changes_exist`
- `spur-worktree::manager::tests_option_e::collect_diff_returns_none_when_no_changes`
- `spur-mcp::plan::tests::build_task_diff_fields_emits_marker_when_diff_none`
- `spur-mcp::plan::tests::build_task_diff_fields_emits_diff_when_present`

### Step F.2: Scope-creep check

Run: `grep -rn "commit_sha" crates/spur-acp/ crates/spur-mcp/` — verify no new `commit_sha` field on `DelegationResult` (rejected Option B).

### Step F.3: LOC check

Run: `git diff <before-T1>..HEAD --stat`
Expected: ~35 LOC across 3 files + ~60 LOC of test scaffolding. If significantly over, investigate.

---

## Rollback

Two task commits on top of main. Revert via `git revert <T2> <T1>` (in that order). T1 is reversible without T2. T2 depends on T1's fallback returning a basis — reverting just T2 leaves the handler with silent omission again but doesn't break anything.

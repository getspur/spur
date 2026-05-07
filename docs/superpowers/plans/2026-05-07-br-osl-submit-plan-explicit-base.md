# br-osl: `submit_plan` Explicit Branch Base — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional `base: BaseTarget` parameter to `submit_plan` (RepoMain | Branch | Commit only — no `WithOverlay` in this scope) so the brain can dispatch a plan against an explicit ref instead of always snapshotting its working tree, fixing the "wrong-base silently dispatched" failure mode and unblocking phased/stacked workflows like `bd-2m2u` Phase 1.

**Architecture:** When `base` is omitted or `RepoMain`, behavior is unchanged (stash-from-WT into `spur/brain-snapshot-*`). When `base` is `Branch{name}` or `Commit{oid}`, resolve to an OID and create `spur/brain-snapshot-*` directly at that OID — no working-tree touch, no stash. The reconciler continues to read `base_snapshot_branch` exactly as today, so dispatch and `merge_plan` semantics are unchanged downstream. The orchestrator's per-dispatch `snapshot_brain_state` is also gated so an explicit base does not re-fail on a dirty WT at dispatch time. The `PlanSubmit` audit sentinel records the operator-supplied `explicit_base` for forensics.

**Tech Stack:** Rust, tokio, serde, schemars (`JsonSchema`), tempfile, the existing `WorktreeManager` git plumbing, and the `__test_call_submit_plan` harness in `tests/common/g_strict_harness.rs`.

**Out of scope (deferred):** `BaseSpec::WithOverlay` at plan level (file follow-up issue once a real use case appears); per-task `base` override; changing the default when `base` is omitted; `execute_epic` parity beyond what falls out of touching the shared `emit_plan_submit_audit` signature.

---

## File Structure

**Create:**
- `crates/spur-mcp/tests/submit_plan_explicit_base.rs` — new integration test file.

**Modify:**
- `crates/spur-worktree/src/manager.rs` — add `snapshot_at_ref` method (creates `spur/brain-snapshot-*` at a given OID, no stash).
- `crates/spur-mcp/src/server.rs`
  - replace `snapshot_plan_base` with `resolve_plan_base` that branches on `Option<&BaseTarget>`;
  - extend `handle_submit_plan` to parse `base` and thread it through;
  - extend `emit_plan_submit_audit` signature with `explicit_base: Option<&BaseTarget>`.
- `crates/spur-mcp/src/tools.rs` — extend `submit_plan_def()` JSON schema with the `base` property.
- `crates/spur-mcp/src/plan/audit_sentinel.rs` — add `explicit_base: Option<BaseTarget>` to the `PlanSubmit` variant.
- `crates/spur-core/src/orchestrator.rs` — gate `snapshot_brain_state` to non-explicit bases only; gate `delete_snapshot_branch` on the same condition.
- `crates/spur-mcp/tests/submit_plan_schema.rs` — schema-shape test for the new `base` advertisement.
- All other call sites of `emit_plan_submit_audit` (call sites enumerated in Task 7).

Each file has one focused responsibility: schema (`tools.rs`), handler (`server.rs::handle_submit_plan`), git plumbing (`manager.rs`), state record (`audit_sentinel.rs`), worker setup (`orchestrator.rs`), tests in dedicated files.

---

## Task 1: Add `BaseTarget::Deserialize` audit (no code change, verify only)

**Files:**
- Read: `crates/spur-mcp/src/tools.rs:18-52`

`BaseTarget` already derives `Serialize + JsonSchema` and has a tolerant manual `Deserialize` impl. No code change needed; this task is a verification step before later tasks rely on round-tripping `BaseTarget` through serde for the audit sentinel.

- [ ] **Step 1: Verify `BaseTarget` round-trips RepoMain / Branch / Commit through `serde_json`**

Run:
```bash
cd /Volumes/Projects/spur && cargo test -p spur-mcp --lib base_target 2>&1 | tail -30
```

If no `base_target` tests exist yet, add this minimal one to `crates/spur-mcp/src/tools.rs` at the very bottom of the file:

```rust
#[cfg(test)]
mod base_target_round_trip {
    use super::BaseTarget;

    #[test]
    fn repo_main_round_trips() {
        let v = serde_json::to_value(BaseTarget::RepoMain).unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(back, BaseTarget::RepoMain);
    }

    #[test]
    fn branch_round_trips() {
        let v = serde_json::to_value(BaseTarget::Branch {
            name: "feature/x".into(),
        })
        .unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(back, BaseTarget::Branch { name: "feature/x".into() });
    }

    #[test]
    fn commit_round_trips() {
        let v = serde_json::to_value(BaseTarget::Commit {
            oid: "abc123".into(),
        })
        .unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(back, BaseTarget::Commit { oid: "abc123".into() });
    }
}
```

- [ ] **Step 2: Run the round-trip tests**

Run: `cargo test -p spur-mcp --lib base_target_round_trip -- --nocapture`
Expected: all three tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "test(spur-mcp): BaseTarget serde round-trip guard for br-osl"
```

---

## Task 2: Add `WorktreeManager::snapshot_at_ref` (TDD)

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:398-477`
- Test: `crates/spur-worktree/src/manager.rs` (new `#[cfg(test)] mod snapshot_at_ref_tests` at file end)

The new method mirrors `snapshot_brain_state` (uses the same `SNAPSHOT_SEQ` counter and timestamp pattern at line 410-413) but creates the snapshot branch directly at a caller-supplied ref — no `git status`, no `stash create`. It is what `resolve_plan_base` will call when the operator supplies `Branch{name}` or `Commit{oid}`.

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-worktree/src/manager.rs`:

```rust
#[cfg(test)]
mod snapshot_at_ref_tests {
    use super::WorktreeManager;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn capture_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[tokio::test]
    async fn snapshot_at_ref_creates_branch_at_named_oid_without_touching_wt() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);
        let main_oid = capture_git(repo, &["rev-parse", "HEAD"]);

        // Branch off main, advance, then jump back so HEAD != target_branch's tip.
        run_git(repo, &["checkout", "-q", "-b", "target"]);
        std::fs::write(repo.join("b"), "2").unwrap();
        run_git(repo, &["add", "b"]);
        run_git(repo, &["commit", "-q", "-m", "second"]);
        let target_oid = capture_git(repo, &["rev-parse", "HEAD"]);
        run_git(repo, &["checkout", "-q", "main"]);

        // Make WT dirty — must NOT cause snapshot_at_ref to fail.
        std::fs::write(repo.join("a"), "dirty").unwrap();

        let manager = WorktreeManager::new(repo.to_path_buf());
        let snap_branch = manager
            .snapshot_at_ref(&target_oid)
            .await
            .expect("snapshot_at_ref must succeed despite dirty WT");

        assert!(
            snap_branch.starts_with("spur/brain-snapshot-"),
            "snapshot branch name must follow convention; got {snap_branch}"
        );
        let snap_oid = capture_git(repo, &["rev-parse", &snap_branch]);
        assert_eq!(
            snap_oid, target_oid,
            "snapshot must point at the requested target OID, not main HEAD ({main_oid}) and not a stash commit"
        );
        // Brain WT untouched (file still says "dirty" because we never stashed).
        let a_contents = std::fs::read_to_string(repo.join("a")).unwrap();
        assert_eq!(a_contents, "dirty");
    }

    #[tokio::test]
    async fn snapshot_at_ref_resolves_branch_name_to_oid() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);
        run_git(repo, &["branch", "feature/x"]);
        let feature_oid = capture_git(repo, &["rev-parse", "feature/x"]);

        let manager = WorktreeManager::new(repo.to_path_buf());
        let snap_branch = manager.snapshot_at_ref("feature/x").await.unwrap();
        let snap_oid = capture_git(repo, &["rev-parse", &snap_branch]);
        assert_eq!(snap_oid, feature_oid);
    }

    #[tokio::test]
    async fn snapshot_at_ref_fails_loudly_on_unknown_ref() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);

        let manager = WorktreeManager::new(repo.to_path_buf());
        let err = manager.snapshot_at_ref("does/not/exist").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does/not/exist") || msg.to_lowercase().contains("unknown"),
            "error must mention the bad ref; got: {msg}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-worktree --lib snapshot_at_ref_tests 2>&1 | tail -40`
Expected: compile error (`no method named snapshot_at_ref`).

- [ ] **Step 3: Implement `snapshot_at_ref`**

Insert this method directly after `snapshot_brain_state` (after `crates/spur-worktree/src/manager.rs:477`):

```rust
    /// Create a `spur/brain-snapshot-*` branch pointed at a caller-supplied
    /// ref (branch name, tag, or commit OID). Unlike `snapshot_brain_state`,
    /// this does not stash the working tree — the brain WT is never touched.
    /// Used by `submit_plan` when the operator passed an explicit `base`.
    pub async fn snapshot_at_ref(&self, target_ref: &str) -> Result<String> {
        // Resolve to an OID first so the snapshot branch is decoupled from
        // any subsequent movement of the source ref.
        let oid = self
            .run_git(&["rev-parse", "--verify", target_ref], None)
            .await
            .with_context(|| format!("failed to resolve ref '{target_ref}'"))?;

        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let branch_name = format!("spur/brain-snapshot-{timestamp}-{seq}");

        self.run_git_with_retry(&["branch", &branch_name, &oid], None, false)
            .await
            .context("failed to create snapshot branch at resolved ref")?;

        Ok(branch_name)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p spur-worktree --lib snapshot_at_ref_tests 2>&1 | tail -20`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-worktree): add snapshot_at_ref (br-osl)

New method creates spur/brain-snapshot-* directly at a caller-supplied
ref without touching the working tree. Submit-time use case for
plan-level explicit base."
```

---

## Task 3: Extend `PlanSubmit` audit sentinel with `explicit_base` (TDD)

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs:72-84`
- Test: `crates/spur-mcp/src/plan/audit_sentinel.rs` (existing `#[cfg(test)] mod tests` if present, else new)

`BaseTarget` must derive `Deserialize` (not just the manual tolerant impl) for serde-tagged-enum macro expansion to compile inside `PlanSubmit`. The manual impl satisfies the `Deserialize` trait already, so no derive change is needed — just use `BaseTarget` directly.

- [ ] **Step 1: Write the failing round-trip test**

Append (or extend the existing test module) at the bottom of `crates/spur-mcp/src/plan/audit_sentinel.rs`:

```rust
#[cfg(test)]
mod plan_submit_explicit_base_round_trip {
    use super::*;
    use crate::tools::BaseTarget;

    #[test]
    fn plan_submit_with_explicit_base_round_trips() {
        let original = AuditSentinelKind::PlanSubmit {
            plan_id: "p1".into(),
            epic_issue_id: "br-1".into(),
            task_ids: vec!["t1".into()],
            base_snapshot_branch: Some("spur/brain-snapshot-x".into()),
            base_snapshot_oid: Some("deadbeef".into()),
            execution_mode: Some("submit_plan".into()),
            brain_session_id: Some("brain-1".into()),
            explicit_base: Some(BaseTarget::Branch {
                name: "spur/plan-merge-phase0".into(),
            }),
        };
        let body = encode_comment(&original);
        let parsed = parse_comment(&body)
            .expect("must parse")
            .expect("must succeed");
        assert_eq!(parsed, original);
    }

    #[test]
    fn plan_submit_omitting_explicit_base_round_trips() {
        let original = AuditSentinelKind::PlanSubmit {
            plan_id: "p1".into(),
            epic_issue_id: "br-1".into(),
            task_ids: vec!["t1".into()],
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            execution_mode: None,
            brain_session_id: None,
            explicit_base: None,
        };
        let body = encode_comment(&original);
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn legacy_plan_submit_without_explicit_base_field_decodes() {
        // Pre-br-osl serialized form (no explicit_base key). serde(default)
        // must let this decode as None.
        let legacy_json = r#"{"kind":"plan-submit","plan_id":"p1","epic_issue_id":"br-1","task_ids":["t1"]}"#;
        let kind: AuditSentinelKind = serde_json::from_str(legacy_json).unwrap();
        match kind {
            AuditSentinelKind::PlanSubmit { explicit_base, .. } => {
                assert!(explicit_base.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-mcp --lib plan_submit_explicit_base 2>&1 | tail -30`
Expected: compile error (`no field 'explicit_base'`).

- [ ] **Step 3: Add `explicit_base` to the `PlanSubmit` variant**

In `crates/spur-mcp/src/plan/audit_sentinel.rs:72-84`, change the `PlanSubmit` arm to:

```rust
    PlanSubmit {
        plan_id: String,
        epic_issue_id: String,
        task_ids: Vec<String>,
        #[serde(default)]
        base_snapshot_branch: Option<String>,
        #[serde(default)]
        base_snapshot_oid: Option<String>,
        #[serde(default)]
        execution_mode: Option<String>,
        #[serde(default)]
        brain_session_id: Option<String>,
        /// Operator-supplied `base` parameter from `submit_plan`.
        /// `None` for legacy submissions and for omitted-base submissions.
        /// Pure forensics — dispatch reads `base_snapshot_branch`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explicit_base: Option<crate::tools::BaseTarget>,
    },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-mcp --lib plan_submit_explicit_base 2>&1 | tail -20`
Expected: 3 tests PASS. The whole crate may fail to compile because `PlanSubmit` is constructed elsewhere — that's expected and fixed in Tasks 6 and 7.

- [ ] **Step 5: Commit (do NOT run other tests yet — call sites still need updating)**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs
git commit -m "feat(spur-mcp): add explicit_base to PlanSubmit audit sentinel (br-osl)

Forensic record of operator-supplied submit_plan base. Backward-compat
via #[serde(default)] for legacy comments without the field."
```

---

## Task 4: Replace `snapshot_plan_base` with `resolve_plan_base` (TDD)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:1696-1725` (function + struct)
- Modify: `crates/spur-mcp/src/server.rs:5012` (single call site inside `handle_submit_plan`)
- Test: new `#[cfg(test)] mod resolve_plan_base_tests` near the function

The new helper takes `Option<&BaseTarget>` instead of just `repo_root`. Behavior:
- `None` or `Some(RepoMain)` → call `WorktreeManager::snapshot_brain_state` (current behavior).
- `Some(Branch{name})` → call `WorktreeManager::snapshot_at_ref(name)`.
- `Some(Commit{oid})` → call `WorktreeManager::snapshot_at_ref(oid)`.

In all cases, `git rev-parse --verify <branch>` resolves the OID; `PlanBaseSnapshot { branch, oid }` stays the same shape.

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-mcp/src/server.rs` (or to an adjacent existing `#[cfg(test)] mod` if one exists for this section):

```rust
#[cfg(test)]
mod resolve_plan_base_tests {
    use super::*;
    use crate::tools::BaseTarget;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
    }
    fn capture(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn seed_repo(repo: &std::path::Path) {
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "seed"]);
    }

    #[tokio::test]
    async fn resolve_plan_base_none_falls_back_to_brain_snapshot() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        let root = dir.path().to_path_buf();

        let snap = resolve_plan_base(Some(&root), None).await.unwrap();
        assert!(snap.branch.as_deref().unwrap().starts_with("spur/brain-snapshot-"));
        assert_eq!(snap.oid.as_deref(), Some(head_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_branch_target_skips_stash_and_uses_named_branch() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        run_git(dir.path(), &["checkout", "-q", "-b", "phase0"]);
        std::fs::write(dir.path().join("b"), "2").unwrap();
        run_git(dir.path(), &["add", "b"]);
        run_git(dir.path(), &["commit", "-q", "-m", "phase0 work"]);
        let phase0_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);

        // Dirty the WT — must not affect snapshot.
        std::fs::write(dir.path().join("a"), "dirty").unwrap();

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch { name: "phase0".into() };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();

        assert_eq!(snap.oid.as_deref(), Some(phase0_oid.as_str()));
        let a_contents = std::fs::read_to_string(dir.path().join("a")).unwrap();
        assert_eq!(a_contents, "dirty", "WT must be untouched");
    }

    #[tokio::test]
    async fn resolve_plan_base_commit_target_uses_oid() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let seed_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("a"), "2").unwrap();
        run_git(dir.path(), &["add", "a"]);
        run_git(dir.path(), &["commit", "-q", "-m", "second"]);
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        assert_ne!(seed_oid, head_oid);

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Commit { oid: seed_oid.clone() };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();
        assert_eq!(snap.oid.as_deref(), Some(seed_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_unknown_branch_fails_loudly() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch { name: "does-not-exist".into() };
        let err = resolve_plan_base(Some(&root), Some(&target)).await.unwrap_err();
        assert!(
            err.contains("does-not-exist"),
            "error must mention the bad ref; got: {err}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-mcp --lib resolve_plan_base_tests 2>&1 | tail -30`
Expected: compile error (`cannot find function 'resolve_plan_base'`).

- [ ] **Step 3: Replace `snapshot_plan_base` with `resolve_plan_base`**

In `crates/spur-mcp/src/server.rs:1696-1719`, replace the entire `snapshot_plan_base` function with:

```rust
async fn resolve_plan_base(
    repo_root: Option<&std::path::PathBuf>,
    base_target: Option<&crate::tools::BaseTarget>,
) -> Result<PlanBaseSnapshot, String> {
    let Some(root) = repo_root.cloned() else {
        return Ok(PlanBaseSnapshot::default());
    };
    let manager = WorktreeManager::new(root);

    let branch = match base_target {
        // Legacy / explicit RepoMain: snapshot the brain working tree.
        None | Some(crate::tools::BaseTarget::RepoMain) => manager
            .snapshot_brain_state()
            .await
            .map_err(|e| format!("failed to snapshot plan base: {e}"))?,
        // Explicit branch: resolve the ref and create a snapshot ref pointed
        // at the same OID. Brain working tree is never touched.
        Some(crate::tools::BaseTarget::Branch { name }) => manager
            .snapshot_at_ref(name)
            .await
            .map_err(|e| format!("failed to resolve plan base branch '{name}': {e}"))?,
        Some(crate::tools::BaseTarget::Commit { oid }) => manager
            .snapshot_at_ref(oid)
            .await
            .map_err(|e| format!("failed to resolve plan base commit '{oid}': {e}"))?,
    };

    let oid = Some(
        run_git_capture(
            &manager.repo_root,
            None,
            &["rev-parse", "--verify", branch.as_str()],
        )
        .await?,
    );
    Ok(PlanBaseSnapshot {
        branch: Some(branch),
        oid,
    })
}
```

- [ ] **Step 4: Update the single call site in `handle_submit_plan`**

In `crates/spur-mcp/src/server.rs:5012`, change:

```rust
        let base_snapshot = match snapshot_plan_base(self.repo_root.as_ref()).await {
```

to (note `explicit_base` is parsed in Task 5; for now, pass `None` so the build is green):

```rust
        // base_target wired up in Task 5; for compile parity, pass None here.
        let base_snapshot = match resolve_plan_base(self.repo_root.as_ref(), None).await {
```

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p spur-mcp --lib resolve_plan_base_tests 2>&1 | tail -20`
Expected: 4 tests PASS.

- [ ] **Step 6: Run full crate compile**

Run: `cargo build -p spur-mcp 2>&1 | tail -10`
Expected: compile success. (`PlanSubmit` constructions must already include `explicit_base: None` if this hits any — fix inline if needed by adding `explicit_base: None` at each construction site; main one is in Task 7.)

If compile fails because of `PlanSubmit { ..., explicit_base }` missing, add `explicit_base: None,` to each construction site in `server.rs` and continue. We will wire the real value in Task 6.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "refactor(spur-mcp): resolve_plan_base helper accepting Option<BaseTarget> (br-osl)

Drops snapshot_plan_base, adds resolve_plan_base which dispatches to
snapshot_brain_state for None/RepoMain or snapshot_at_ref for Branch/
Commit. Call site passes None for now; submit_plan handler wires the
real value in the next task."
```

---

## Task 5: Parse `base` in `handle_submit_plan` (TDD)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:4876-5024` (handler body)
- Modify: `crates/spur-mcp/src/tools.rs:818-877` (schema definition)

The handler must parse the optional `base` field as `Option<BaseTarget>` using the same tolerant deserialization that `delegate_to_worker` enjoys. We deserialize with `serde_json::from_value(args["base"].clone())` because the type's manual tolerant impl already accepts both object and JSON-stringified-object forms.

- [ ] **Step 1: Add the schema property**

In `crates/spur-mcp/src/tools.rs:818-877`, modify `submit_plan_def()` to add a `base` property. Replace the `properties` block of the schema (lines 824-873) with:

```rust
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task_id": {
                                "type": "string",
                                "description": "Unique identifier for this task (use issue ID or descriptive slug)"
                            },
                            "agent": {
                                "type": "string",
                                "description": "Worker agent to execute this task"
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description (CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT)"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "task_ids that must complete before this task starts. Empty or omitted = ready immediately."
                            },
                            "issue_id": {
                                "type": "string",
                                "description": "Optional beads issue ID to auto-track"
                            },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional file paths for worker context"
                            }
                        },
                        "required": ["task_id", "agent", "task"]
                    },
                    "description": "Tasks with dependency edges forming a DAG. Tasks with no depends_on are dispatched immediately."
                },
                "persist_as_epic": {
                    "type": "boolean",
                    "description": "When true, mirror the plan into beads as an epic with child issues + dependency edges. Each child is labeled `spur:plan-id:<plan_id>` so review_task(approve) can auto-close the matching beads issue. Requires `epic_title` and a beads PM backend. Defaults to false (ephemeral in-memory plan only)."
                },
                "epic_title": {
                    "type": "string",
                    "description": "Epic title. Required when `persist_as_epic` is true. Ignored otherwise."
                },
                "epic_body": {
                    "type": "string",
                    "description": "Epic description / rationale. Optional when `persist_as_epic` is true. Ignored otherwise."
                },
                "base": {
                    "description": "Optional explicit base for the plan. Omit (or pass {\"kind\":\"repo_main\"}) for legacy behavior — the plan engine snapshots the brain working tree HEAD. Pass {\"kind\":\"branch\",\"name\":\"<branch>\"} or {\"kind\":\"commit\",\"oid\":\"<oid>\"} to base the plan on a named ref instead; the brain working tree is not touched. Useful for stacking plans on a prior phase's integration branch.",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "kind": { "const": "repo_main" } },
                            "required": ["kind"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "branch" },
                                "name": { "type": "string" }
                            },
                            "required": ["kind", "name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "commit" },
                                "oid": { "type": "string" }
                            },
                            "required": ["kind", "oid"]
                        }
                    ]
                }
            },
            "required": ["tasks"]
        }),
```

- [ ] **Step 2: Write the failing handler test**

Add to `crates/spur-mcp/tests/submit_plan_schema.rs` (after the existing tests):

```rust
#[test]
fn schema_advertises_base_oneof() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("base"))
        .expect("base must be advertised");
    let one_of = prop
        .get("oneOf")
        .and_then(|v| v.as_array())
        .expect("base must be a oneOf union");
    assert_eq!(one_of.len(), 3, "base must list repo_main / branch / commit");
    let kinds: Vec<&str> = one_of
        .iter()
        .filter_map(|variant| {
            variant
                .get("properties")
                .and_then(|p| p.get("kind"))
                .and_then(|k| k.get("const"))
                .and_then(|c| c.as_str())
        })
        .collect();
    assert!(kinds.contains(&"repo_main"));
    assert!(kinds.contains(&"branch"));
    assert!(kinds.contains(&"commit"));
}

#[test]
fn schema_base_field_is_optional() {
    let schema = submit_plan_def();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(!required.contains(&"base"), "base must not be required");
}
```

- [ ] **Step 3: Run schema tests to verify they pass**

Run: `cargo test -p spur-mcp --test submit_plan_schema 2>&1 | tail -20`
Expected: all tests in `submit_plan_schema.rs` PASS (the new ones included).

- [ ] **Step 4: Wire `base` into the handler body**

In `crates/spur-mcp/src/server.rs:4876-5024`, locate the line `let plan_id = uuid::Uuid::new_v4().to_string();` (around line 4940) and immediately AFTER it (so it lives outside the persist_as_epic gating but before any snapshot), add:

```rust
        // Parse optional explicit base. Tolerant: `BaseTarget`'s manual
        // Deserialize accepts both `{"kind":...}` and JSON-stringified-object.
        let explicit_base: Option<crate::tools::BaseTarget> = match args.get("base") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match serde_json::from_value::<crate::tools::BaseTarget>(v.clone()) {
                Ok(target) => Some(target),
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan: invalid 'base' parameter: {e}"),
                    );
                }
            },
        };
```

Then change the `resolve_plan_base` call at line 5012 from:

```rust
        let base_snapshot = match resolve_plan_base(self.repo_root.as_ref(), None).await {
```

to:

```rust
        let base_snapshot = match resolve_plan_base(self.repo_root.as_ref(), explicit_base.as_ref()).await {
```

- [ ] **Step 5: Build and run schema tests again**

Run: `cargo build -p spur-mcp 2>&1 | tail -10 && cargo test -p spur-mcp --test submit_plan_schema 2>&1 | tail -10`
Expected: build success; schema tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/submit_plan_schema.rs
git commit -m "feat(spur-mcp): submit_plan accepts optional 'base' BaseTarget (br-osl)

- Schema advertises base oneOf {repo_main, branch, commit}
- Handler parses tolerantly via BaseTarget's manual Deserialize
- Wired through resolve_plan_base; backward-compat: omit -> brain snapshot

The explicit_base value still needs to be threaded into the audit
sentinel; that lands in the next task along with the call-site sweep."
```

---

## Task 6: Thread `explicit_base` through `emit_plan_submit_audit` (TDD)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:810-838` (function signature)
- Modify: `crates/spur-mcp/src/server.rs:5039-5048` (handle_submit_plan call site)

- [ ] **Step 1: Update the function signature**

In `crates/spur-mcp/src/server.rs:810-838`, change `emit_plan_submit_audit` to:

```rust
pub async fn emit_plan_submit_audit(
    advanced: &dyn spur_pm::BeadsAdvanced,
    plan_id: &str,
    sg: &EpicSubgraph,
    base_snapshot_branch: Option<&str>,
    base_snapshot_oid: Option<&str>,
    execution_mode: Option<&str>,
    brain_session_id: Option<&SessionId>,
    explicit_base: Option<&crate::tools::BaseTarget>,
) {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
        plan_id: plan_id.to_string(),
        epic_issue_id: sg.epic_id.clone(),
        task_ids: sg.task_map.values().cloned().collect(),
        base_snapshot_branch: base_snapshot_branch.map(str::to_string),
        base_snapshot_oid: base_snapshot_oid.map(str::to_string),
        execution_mode: execution_mode.map(str::to_string),
        brain_session_id: brain_session_id.map(ToString::to_string),
        explicit_base: explicit_base.cloned(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = advanced.add_comment(&sg.epic_id, &body).await {
        tracing::warn!(
            target: "spur.audit.emit_failure",
            kind = "plan_submit",
            epic_id = %sg.epic_id,
            plan_id = %plan_id,
            "PlanSubmit audit comment emission failed (graph is persisted; audit missing): {e}"
        );
    }
}
```

- [ ] **Step 2: Update the `handle_submit_plan` call site**

In `crates/spur-mcp/src/server.rs:5039-5048`, change:

```rust
                emit_plan_submit_audit(
                    adv,
                    &plan_id,
                    sg,
                    base_snapshot_branch.as_deref(),
                    base_snapshot_oid.as_deref(),
                    Some("submit_plan"),
                    Some(self.brain_session_id().as_session_id()),
                )
                .await;
```

to:

```rust
                emit_plan_submit_audit(
                    adv,
                    &plan_id,
                    sg,
                    base_snapshot_branch.as_deref(),
                    base_snapshot_oid.as_deref(),
                    Some("submit_plan"),
                    Some(self.brain_session_id().as_session_id()),
                    explicit_base.as_ref(),
                )
                .await;
```

- [ ] **Step 3: Build the crate**

Run: `cargo build -p spur-mcp 2>&1 | tail -30`
Expected: compile errors only at remaining `emit_plan_submit_audit` call sites listed in Task 7. The current task's call site builds.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): thread explicit_base through emit_plan_submit_audit (br-osl)

Adds explicit_base parameter to the helper and wires the submit_plan
handler. Other callers updated in the next task."
```

---

## Task 7: Update remaining `emit_plan_submit_audit` callers

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:5488` (one call)
- Modify: `crates/spur-mcp/src/server.rs:8075`
- Modify: `crates/spur-mcp/src/server.rs:8277`
- Modify: `crates/spur-mcp/src/server.rs:8513`
- Modify: `crates/spur-mcp/src/server.rs:9725`
- Modify: `crates/spur-mcp/src/server.rs:9778`
- Modify: `crates/spur-mcp/tests/submit_plan_audit.rs:171-180`

These are `execute_epic` recovery / restart paths that don't have an `explicit_base` to pass — they reconstruct the plan from persisted state. Pass `None` at all of them. The forensic record is already on the original `PlanSubmit` sentinel from `submit_plan`; recovery emits do not need to re-record it.

- [ ] **Step 1: List the call sites that fail compilation**

Run: `cargo build -p spur-mcp 2>&1 | grep -E "this function takes|emit_plan_submit_audit" | head -20`
Expected: a list of mismatched-arity errors at the lines above.

- [ ] **Step 2: Add `None` as the trailing argument at each call site**

For each of `server.rs` lines 5488, 8075, 8277, 8513, 9725, 9778: insert `None` (with appropriate trailing comma handling) as the new trailing argument before `.await`. Each call already terminates with `Some(...)` or `None` for the brain-session arg followed by `).await;`. Use this pattern (illustrated for line 8075):

Before:
```rust
        crate::emit_plan_submit_audit(
            adv,
            &plan_id,
            &subgraph,
            None,
            None,
            Some("execute_epic_recovery"),
            None,
        )
        .await;
```

After:
```rust
        crate::emit_plan_submit_audit(
            adv,
            &plan_id,
            &subgraph,
            None,
            None,
            Some("execute_epic_recovery"),
            None,
            None,
        )
        .await;
```

(The exact existing arguments differ between sites; only the trailing `None,` is added.)

- [ ] **Step 3: Update the test call site in `tests/submit_plan_audit.rs:171-180`**

Change:

```rust
    spur_mcp::emit_plan_submit_audit(
        adv,
        "P1",
        &subgraph,
        None,
        None,
        None,
        Some(&spur_acp::SessionId("brain-1".into())),
    )
    .await;
```

to:

```rust
    spur_mcp::emit_plan_submit_audit(
        adv,
        "P1",
        &subgraph,
        None,
        None,
        None,
        Some(&spur_acp::SessionId("brain-1".into())),
        None,
    )
    .await;
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build -p spur-mcp 2>&1 | tail -5`
Expected: clean build.

Run: `cargo test -p spur-mcp --test submit_plan_audit 2>&1 | tail -20`
Expected: PASS (skip on environments without `br` is acceptable per harness convention).

Run: `cargo test -p spur-mcp --test submit_plan_schema 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/submit_plan_audit.rs
git commit -m "fix(spur-mcp): pass None for explicit_base from execute_epic recovery paths (br-osl)

Recovery paths reconstruct PlanSubmit sentinels from persisted state and
have no operator-supplied base to record. The original submit_plan call
captured the explicit_base when the plan was first submitted."
```

---

## Task 8: Gate orchestrator's per-dispatch snapshot to non-explicit base only (TDD)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:7986-8018`
- Test: `crates/spur-core/src/orchestrator.rs` (extend the existing `resolve_base_branch_*` test module near line 10627)

The bug: `snapshot_brain_state` is called unconditionally before `resolve_base_branch`. When the brain WT is dirty AND `ctx.base = Some(Branch{...})`, the snapshot can fail (per br-osl) even though we never use it. Gate the snapshot to cases where it's actually consumed.

`snapshot_brain_state` is consumed only when the resolved base falls back to `snapshot_branch` — i.e. when `ctx.base` is `None`, `Some(RepoMain)`, or `Some(WithOverlay { base: RepoMain, .. })`.

- [ ] **Step 1: Write the failing test**

Find the existing `mod` containing `resolve_base_branch_unwraps_with_overlay` near `crates/spur-core/src/orchestrator.rs:10627`. Add a helper that classifies whether a snapshot is needed:

```rust
    #[test]
    fn snapshot_needed_for_none_and_repo_main() {
        // None
        assert!(snapshot_required_for_dispatch(None));
        // RepoMain
        assert!(snapshot_required_for_dispatch(Some(&BaseSpec::RepoMain)));
        // WithOverlay base: RepoMain
        assert!(snapshot_required_for_dispatch(Some(&BaseSpec::WithOverlay {
            base: BaseTarget::RepoMain,
            overlays: vec![],
        })));
    }

    #[test]
    fn snapshot_not_needed_for_branch_or_commit() {
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::Branch {
            name: "x".into()
        })));
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::Commit {
            oid: "abc".into()
        })));
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::WithOverlay {
            base: BaseTarget::Branch { name: "x".into() },
            overlays: vec![],
        })));
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::WithOverlay {
            base: BaseTarget::Commit { oid: "abc".into() },
            overlays: vec![],
        })));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --lib snapshot_needed_for 2>&1 | tail -20 && cargo test -p spur-core --lib snapshot_not_needed_for 2>&1 | tail -20`
Expected: compile error (`cannot find function 'snapshot_required_for_dispatch'`).

- [ ] **Step 3: Add the helper next to `resolve_base_branch`**

Insert after `extract_overlays` at `crates/spur-core/src/orchestrator.rs:135`:

```rust
/// Whether the dispatch path needs to call `snapshot_brain_state`.
/// Required only when the resolved base would fall back to the snapshot
/// branch — i.e. `None` / `RepoMain` / `WithOverlay { base: RepoMain }`.
/// Explicit `Branch` / `Commit` bases consume no snapshot, so taking one
/// just to throw it away is wasted work and (per br-osl) actively breaks
/// dispatch when the brain WT is dirty.
fn snapshot_required_for_dispatch(spec: Option<&BaseSpec>) -> bool {
    match spec {
        None => true,
        Some(BaseSpec::RepoMain) => true,
        Some(BaseSpec::Branch { .. }) | Some(BaseSpec::Commit { .. }) => false,
        Some(BaseSpec::WithOverlay { base, .. }) => matches!(base, BaseTarget::RepoMain),
    }
}
```

- [ ] **Step 4: Run the helper tests**

Run: `cargo test -p spur-core --lib snapshot 2>&1 | tail -20`
Expected: both tests PASS.

- [ ] **Step 5: Gate the call in the dispatch path**

Replace `crates/spur-core/src/orchestrator.rs:7988-8018` (from `// 1. Snapshot brain state...` through the `delete_snapshot_branch` block) with:

```rust
    // 1. Snapshot brain state — only when the resolved base would consume it.
    //    Explicit Branch/Commit bases bypass the WT entirely (br-osl).
    let snapshot_needed = snapshot_required_for_dispatch(ctx.base.as_ref());
    let snapshot_branch = if snapshot_needed {
        worktrees
            .snapshot_brain_state()
            .await
            .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?
    } else {
        String::new()
    };

    let base_branch = ctx
        .base
        .as_ref()
        .map(|spec| resolve_base_branch(spec, &snapshot_branch))
        .unwrap_or_else(|| snapshot_branch.clone());

    let worktree_info = worktrees
        .create_worktree_v2(
            ctx.brain_session_id,
            &worker_session,
            ctx.agent,
            &base_branch,
        )
        .await
        .map_err(|e| AttemptSetupError::WorktreeFailed(e.to_string()))?;

    // The snapshot branch is only needed as a base ref for worktree creation.
    // Once the worktree exists, delete it immediately to prevent ref leaks.
    // Skip when no snapshot was taken in the first place.
    if snapshot_needed {
        if let Err(e) = worktrees.delete_snapshot_branch(&snapshot_branch).await {
            tracing::debug!(
                snapshot_branch = %snapshot_branch,
                error = %e,
                "failed to delete snapshot branch after worktree creation; will leak until cleanup_orphans runs"
            );
        }
    }
```

- [ ] **Step 6: Build and run unit tests**

Run: `cargo build -p spur-core 2>&1 | tail -10`
Expected: clean build.

Run: `cargo test -p spur-core --lib resolve_base_branch 2>&1 | tail -20`
Expected: existing `resolve_base_branch_*` tests still PASS along with the two new ones.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "fix(spur-core): skip brain snapshot at dispatch when base is explicit (br-osl)

Per br-osl, a dirty brain WT was failing dispatch even when ctx.base
was Some(Branch{x}) — the snapshot was taken and immediately discarded,
but its failure aborted the dispatch. Gate snapshot_brain_state to only
the cases that actually consume it (None / RepoMain)."
```

---

## Task 9: End-to-end integration test (TDD, dirty WT + explicit branch base)

**Files:**
- Create: `crates/spur-mcp/tests/submit_plan_explicit_base.rs`

This test exercises the whole pipeline from `__test_call_submit_plan` through to the captured `DelegationRequest`, with a dirty brain WT and an explicit branch base. It mirrors the structure of `tests/submit_plan_audit.rs` but uses the harness from `tests/common/g_strict_harness.rs` and intercepts the dispatched `DelegationRequest` to assert the worker was sent to the explicit ref's HEAD.

- [ ] **Step 1: Read the harness contract**

Open `crates/spur-mcp/tests/common/g_strict_harness.rs` and skim:
- `init_repo` (line 50) seeds the repo
- `TestHarness::new` (line 144) wires server + reconciler + delegation channel
- `submit_plan_with_tasks` (line 265) calls `__test_call_submit_plan`
- `request_rx` is the `mpsc::Receiver<DelegationRequest>` you can drain

The harness already locks CWD; do NOT spawn a second `TestHarness` in parallel within the same test process.

- [ ] **Step 2: Write the failing test**

Create `crates/spur-mcp/tests/submit_plan_explicit_base.rs`:

```rust
//! Integration: `submit_plan` with an explicit `base: Branch{...}` succeeds
//! even when the brain WT is dirty, and the resulting plan dispatches off
//! the explicit branch's HEAD — not a stash-derived snapshot.

use std::process::Command;

use serde_json::{json, Value};
use spur_mcp::tools::{BaseSpec, BaseTarget};

mod common;

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn submit_plan_explicit_branch_base_with_dirty_wt() {
    let mut h = common::g_strict_harness::TestHarness::new().await;
    let repo = h.repo_root();

    // Build a 'phase0' branch with a known commit, then leave HEAD on main.
    run_git(&repo, &["checkout", "-q", "-b", "phase0"]);
    std::fs::write(repo.join("phase0.txt"), "phase0\n").unwrap();
    run_git(&repo, &["add", "phase0.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "phase0 work"]);
    let phase0_oid = run_git(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", "main"]);

    // Dirty the working tree — would crash legacy brain-stash.
    std::fs::write(repo.join("brain_wt_dirty.txt"), "dirty\n").unwrap();
    // Also leave a tracked file modification (porcelain-visible without ??).
    let readme = repo.join("README.md");
    std::fs::write(&readme, "dirty seed\n").unwrap();

    let response: Value = h
        .server_test_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "br-osl explicit base test",
            "base": { "kind": "branch", "name": "phase0" },
            "tasks": [
                { "task_id": "T1", "agent": "mock", "task": "do work", "depends_on": [] }
            ]
        }))
        .await;

    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "submit_plan must succeed with explicit base + dirty WT; got: {response}"
    );

    // Tick the reconciler so T1 is dispatched.
    h.tick_until_request_or_timeout().await;
    let request = h
        .take_next_dispatch()
        .expect("reconciler must dispatch T1 with the explicit base");

    match request.base.as_ref().expect("base must be Some") {
        BaseSpec::WithOverlay {
            base: BaseTarget::Branch { name },
            overlays,
        } => {
            assert!(
                name.starts_with("spur/brain-snapshot-"),
                "reconciler still wraps the snapshot ref; got {name}"
            );
            assert!(
                overlays.is_empty(),
                "T1 has no approved deps so no overlays expected; got {overlays:?}"
            );
            // The snapshot ref must point at phase0's OID, not at main HEAD.
            let snap_oid = run_git(&repo, &["rev-parse", "--verify", name]);
            assert_eq!(
                snap_oid, phase0_oid,
                "explicit base must materialize as a snapshot ref pointing at the named branch's OID"
            );
        }
        other => panic!("unexpected base shape: {other:?}"),
    }
}

#[tokio::test]
async fn submit_plan_unknown_base_branch_returns_error() {
    let mut h = common::g_strict_harness::TestHarness::new().await;

    let response: Value = h
        .server_test_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "br-osl bad base",
            "base": { "kind": "branch", "name": "does-not-exist" },
            "tasks": [
                { "task_id": "T1", "agent": "mock", "task": "do work", "depends_on": [] }
            ]
        }))
        .await;

    let err = response.get("error").cloned().unwrap_or(Value::Null);
    assert!(
        !err.is_null(),
        "submit_plan must reject unknown base branch; got: {response}"
    );
    let msg = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("does-not-exist") || msg.contains("base"),
        "error message must mention the bad ref or 'base'; got: {msg}"
    );
}
```

- [ ] **Step 3: Add the helper accessors the test needs**

The test uses `h.server_test_submit_plan`, `h.tick_until_request_or_timeout`, and `h.take_next_dispatch`. If these methods do not exist on `TestHarness`, add them as small `pub` wrappers in `crates/spur-mcp/tests/common/g_strict_harness.rs` (after `submit_plan_with_tasks` near line 277):

```rust
    pub async fn server_test_submit_plan(&self, args: serde_json::Value) -> serde_json::Value {
        self.server.__test_call_submit_plan(args).await
    }

    pub async fn tick_until_request_or_timeout(&mut self) {
        // Drive the reconciler one tick; bound by a generous 2s budget so the
        // mpsc channel populates before we read.
        let _ = self.reconciler.__test_tick_once().await;
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.request_rx.recv(),
        )
        .await
        .ok()
        .flatten()
        .map(|req| self.pending_requests.push_back(req));
    }

    pub fn take_next_dispatch(&mut self) -> Option<DelegationRequest> {
        self.pending_requests.pop_front()
    }
```

If `Reconciler::__test_tick_once` does not exist, search the existing harness for whatever it currently uses to drive ticks (look for `fn tick` or `fast_forward`) and substitute that name. The intent is "advance the reconciler until T1 is dispatched."

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p spur-mcp --test submit_plan_explicit_base 2>&1 | tail -50`
Expected: both tests PASS.

If `tick_until_request_or_timeout` returns nothing (T1 not dispatched), increase the timeout to 2 seconds or add `h.reconciler.fast_forward_now()` (or whatever the harness exposes) before `request_rx.recv()`.

- [ ] **Step 5: Run the rest of the spur-mcp test suite to confirm no regressions**

Run: `cargo test -p spur-mcp 2>&1 | tail -30`
Expected: all tests PASS (any `submit_plan_audit` skips on missing `br` are fine).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/tests/submit_plan_explicit_base.rs crates/spur-mcp/tests/common/g_strict_harness.rs
git commit -m "test(spur-mcp): submit_plan with explicit branch base survives dirty WT (br-osl)

Two integration tests:
- happy path: dirty WT + base: branch:phase0 dispatches at phase0's OID
- error path: base: branch:does-not-exist returns invalid_params"
```

---

## Task 10: Documentation

**Files:**
- Modify: `docs/architecture-spur-mcp.md` (find the `submit_plan` section)

- [ ] **Step 1: Locate the section**

Run: `rg -n '^##.*submit_plan|^### submit_plan' docs/architecture-spur-mcp.md`
Expected: at least one heading match. Open the file at that line.

- [ ] **Step 2: Insert a "Plan-level base" subsection**

Under the `submit_plan` section, append:

````markdown
### Plan-level base (br-osl)

`submit_plan` accepts an optional `base: BaseTarget` parameter. When omitted (or set to `{"kind":"repo_main"}`), the plan engine snapshots the brain working tree HEAD into `spur/brain-snapshot-*` (legacy default — convenient for "extend my desk" workflows).

To dispatch a plan against an explicit ref instead, pass:

```json
{ "tasks": [...], "base": { "kind": "branch", "name": "<branch>" } }
```

or

```json
{ "tasks": [...], "base": { "kind": "commit", "oid": "<oid>" } }
```

In these explicit cases:
- The brain working tree is **not** touched (no stash, no `index.lock` contention).
- A `spur/brain-snapshot-*` ref is created pointing at the resolved OID, decoupling the plan's base from any later movement of the source branch.
- `merge_plan` cherry-picks worker branches onto this snapshot ref exactly as before.
- The reconciler still emits `WithOverlay { base: Branch{<snapshot ref>}, overlays: [<approved deps>] }` for every dispatch.
- The `PlanSubmit` audit sentinel records the operator-supplied `BaseTarget` in `explicit_base` for forensics.

Use case: stacking phased plans. Phase N+1 specifies `base: { kind: "branch", name: "spur/plan-merge-<phase-N-id>" }` so its workers see Phase N's approved-but-unmerged work as their foundation.

Out of scope (not in this implementation): plan-level `WithOverlay`, per-task `base` overrides. File a follow-up issue if either becomes necessary.
````

- [ ] **Step 3: Cross-reference from `bd-2m2u` RCA Phase ordering note (if present)**

Run: `rg -l 'bd-2m2u' docs/`
If a Phase ordering note exists, append a line: "See `br-osl` for the explicit-base mechanism added to `submit_plan` for stacked phases."

- [ ] **Step 4: Commit**

```bash
git add docs/architecture-spur-mcp.md
git commit -m "docs(spur-mcp): document submit_plan explicit base parameter (br-osl)"
```

---

## Self-Review

**1. Spec coverage** (br-osl acceptance criteria):
- ✅ `submit_plan` accepts optional `base: BaseSpec` at plan level → Task 5 (note: scoped to `BaseTarget`, not full `BaseSpec`; deferred per Path B decision; documented in plan goal).
- ⚠️ Per-task `base` → out of scope by design (Path B). Documented in plan header.
- ✅ When `base` is `Branch{name}` or `Commit{oid}`, the plan engine does NOT touch the brain working tree → Task 4 (`resolve_plan_base`) + Task 8 (orchestrator gate).
- ✅ `merge.base_snapshot_branch` reflects the explicit base → Task 4 stores it in `PlanState`.
- ✅ Reconciler dispatch path threads the resolved base → unchanged; `plan_dispatch_base_spec` already wraps `base_snapshot_branch`.
- ✅ Backwards-compat → Task 4 default-None branch.
- ✅ Integration test: dirty WT + explicit branch → Task 9.
- ✅ Integration test: phase-N integration as base → Task 9 (`phase0` synthetic).
- ✅ Documentation → Task 10.

**2. Placeholder scan:** No "TBD", "implement later", or "similar to Task N" patterns. Each task contains the full code to write/test/run.

**3. Type consistency:** `BaseTarget` is the type used everywhere (schema, handler parsing, `resolve_plan_base`, `audit_sentinel::PlanSubmit::explicit_base`). The reconciler keeps using `BaseSpec::WithOverlay { base: BaseTarget::Branch { name } }` because that's the dispatch-time shape — unchanged. `emit_plan_submit_audit` takes `Option<&BaseTarget>` (Task 6) and all call sites pass either `explicit_base.as_ref()` (Task 6) or `None` (Task 7) — consistent.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-07-br-osl-submit-plan-explicit-base.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch with checkpoints.

Which approach?

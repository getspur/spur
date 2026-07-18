# Runtime Skill Projection Reconciler Final Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve every user-owned projection symlink through generation garbage collection and close the two materialization JSONL compatibility gaps found in final review.

**Architecture:** Build on the rejected attempt-3 branch, which already contains the approved resolver, generation, reconciliation, exact-hint, locking, and migration work. Extend the existing preservation set at the remaining prefixless legacy skip seam, and make the JSONL upgrader distinguish an untouched empty legacy record from a fully retired record while preserving record boundaries during append.

**Tech Stack:** Rust 2021, Tokio tests, `fs4` locking, `serde_json`, `tempfile`, workspace `scripts/spur-cargo` wrapper.

---

## Locked scope and invariants

- Base this work on `spur/worker/v2/codex/30940b0bf86d507f/de77667e-9d4f-472d-9355-38d948d0574a` (normalized head `ac2865d4268b42bd7dda05699410dd6951e469aa`). Do not reconstruct or replace the prior recovery work.
- Modify only:
  - `crates/spur-core/src/skills/projection/reconcile.rs`
  - `crates/spur-core/src/explore/materialize.rs`
- Keep all public reconciliation signatures unchanged.
- Do not change manifests, adapters, installer marker rules, dependencies, CLI/TUI/brain/worker integration, or unrelated files.
- Preserve strict legacy ownership classification. Retention protects a skipped target's referent; it does not adopt that target.
- Keep one shared materialization lock and occurrence-exact UUID retirement semantics unchanged.
- Preserve malformed JSONL bytes and valid empty legacy records exactly.
- Use two commits: a failing regression commit first, then the minimal fix commit. Do not squash them in the worker worktree.

### Task 1: Add the three final regressions

**Files:**
- Modify and test: `crates/spur-core/src/skills/projection/reconcile.rs`
- Modify and test: `crates/spur-core/src/explore/materialize.rs`

- [ ] **Step 1: Add a prefixless preserved-symlink generation regression**

Add a Unix Tokio test beside `fresh_user_symlink_keeps_its_referenced_generation_alive`. The test must exercise `plan_legacy_pool_removals`, not the already-covered desired-target collision branch:

```rust
#[cfg(unix)]
#[tokio::test]
async fn prefixless_user_symlink_keeps_its_referenced_generation_alive() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_pool_skill("legacy-link", "clean", "OLD");
    let first = publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
    let target = legacy_target(Adapter::Codex, fixture.launch_root(), "legacy-link").unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    NativeLinker
        .symlink(
            &relative_symlink_source(&first.root.join("skills/legacy-link"), &target).unwrap(),
            &target,
            TargetKind::Directory,
        )
        .unwrap();
    append_materialization_record(
        fixture.repo_root(),
        &MaterializationRecord {
            recorded_at_epoch: 1,
            delegation_id: "legacy-link".into(),
            agent: "codex".into(),
            worktree: fixture.launch_root().display().to_string(),
            items: vec!["legacy-link".into()],
        },
    )
    .unwrap();
    fixture.write_pool_skill("legacy-link", "clean", "NEW");

    let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();

    assert!(summary.skipped.iter().any(|skip| {
        skip.skill_id == "legacy-link" && skip.reason == ProjectionSkipReason::UserOwned
    }));
    assert!(path_exists_no_follow(&target).unwrap());
    assert!(first.root.exists());
    assert!(std::fs::canonicalize(&target).unwrap().starts_with(&first.root));
}
```

The old and new pool bodies must produce distinct generation digests so the assertion proves garbage-collection retention rather than accidentally testing the current generation.

- [ ] **Step 2: Add exact preservation coverage for an empty legacy JSONL record**

Add a unit test in `explore::materialize::tests` that writes an empty-item legacy record between malformed lines, invokes `legacy_materialization_hints`, and compares the file byte-for-byte:

```rust
#[test]
fn hint_snapshot_preserves_empty_legacy_record_and_malformed_lines() {
    let repo = tempfile::tempdir().unwrap();
    let worktree = repo.path().join("worker");
    let cache = repo.path().join(".spur/explore/cache");
    std::fs::create_dir_all(&cache).unwrap();
    let empty = MaterializationRecord {
        recorded_at_epoch: 1,
        delegation_id: "empty".into(),
        agent: "codex".into(),
        worktree: worktree.display().to_string(),
        items: vec![],
    };
    let raw = format!(
        "not-json-before\n{}\nnot-json-after\n",
        serde_json::to_string(&empty).unwrap()
    );
    let path = cache.join("materializations.jsonl");
    std::fs::write(&path, &raw).unwrap();

    assert!(legacy_materialization_hints(repo.path(), &worktree).is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
}
```

- [ ] **Step 3: Add a no-final-EOL append compatibility regression**

Write one valid record without a trailing newline, append a second record through `append_materialization_record`, then assert both records remain independently readable and the bytes contain a separator:

```rust
#[test]
fn append_separates_an_existing_record_without_final_newline() {
    let repo = tempfile::tempdir().unwrap();
    let cache = repo.path().join(".spur/explore/cache");
    std::fs::create_dir_all(&cache).unwrap();
    let first = MaterializationRecord {
        recorded_at_epoch: 1,
        delegation_id: "first".into(),
        agent: "codex".into(),
        worktree: "/worker".into(),
        items: vec!["first-skill".into()],
    };
    let second = MaterializationRecord {
        recorded_at_epoch: 2,
        delegation_id: "second".into(),
        agent: "codex".into(),
        worktree: "/worker".into(),
        items: vec!["second-skill".into()],
    };
    let path = cache.join("materializations.jsonl");
    std::fs::write(&path, serde_json::to_string(&first).unwrap()).unwrap();

    append_materialization_record(repo.path(), &second).unwrap();

    let records = read_recent_materializations(repo.path(), 10);
    assert_eq!(records, vec![second, first]);
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
}
```

- [ ] **Step 4: Run the new tests and prove RED**

Run:

```bash
scripts/spur-cargo test -p spur-core prefixless_user_symlink_keeps_its_referenced_generation_alive -- --nocapture
scripts/spur-cargo test -p spur-core hint_snapshot_preserves_empty_legacy_record_and_malformed_lines -- --nocapture
scripts/spur-cargo test -p spur-core append_separates_an_existing_record_without_final_newline -- --nocapture
```

Expected before implementation:

- The prefixless symlink test fails because the old generation directory is removed or canonicalization fails.
- The empty-record test fails because snapshot upgrade drops the empty record.
- The append test fails because the two JSON objects are concatenated and only zero/one valid record is read.

- [ ] **Step 5: Commit only the failing regressions**

```bash
git add crates/spur-core/src/skills/projection/reconcile.rs crates/spur-core/src/explore/materialize.rs
git commit -m "test(skills): rsp-4rf cover final recovery gaps"
```

### Task 2: Retain prefixless generations and preserve JSONL compatibility

**Files:**
- Modify: `crates/spur-core/src/skills/projection/reconcile.rs`
- Modify: `crates/spur-core/src/explore/materialize.rs`

- [ ] **Step 1: Thread generation retention into legacy-pool removal planning**

Extend only the private helper and call site:

```rust
fn plan_legacy_pool_removals(
    request: ProjectionRequest<'_>,
    projection_root: &Path,
    legacy_skill_ids: &[String],
    processed: &mut HashSet<String>,
    operations: &mut Vec<PlannedOperation>,
    preserved_generations: &mut BTreeSet<String>,
    summary: &mut ProjectionSummary,
) -> Result<(), ReconcileFailure>
```

At the `build_reconciliation_plan` call, pass `projection_root` and `&mut preserved_generations`. In both branches that preserve an existing target, call the existing helper before recording the skip:

```rust
crate::skills::installer::LegacyMarkerOwnership::UserEdited => {
    retain_generation_reference_for_target(
        projection_root,
        &target,
        preserved_generations,
    )
    .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
    summary.skipped.push(ProjectionSkip {
        skill_id: skill_id.clone(),
        path: target,
        reason: ProjectionSkipReason::UserEdited,
    });
}
_ => {
    retain_generation_reference_for_target(
        projection_root,
        &target,
        preserved_generations,
    )
    .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
    summary.skipped.push(ProjectionSkip {
        skill_id: skill_id.clone(),
        path: target,
        reason: ProjectionSkipReason::UserOwned,
    });
}
```

Do not retain a generation for a managed target scheduled for removal, and do not weaken `legacy_marker_ownership`.

- [ ] **Step 2: Leave empty legacy records byte-for-byte unchanged during hint upgrade**

In `legacy_materialization_hints`, skip UUID upgrade for records with no items because they produce no hint and can never be retired by item identity:

```rust
if record.items.is_empty() {
    continue;
}
let needs_fresh_ids = item_ids.as_ref().is_none_or(|ids| {
    ids.iter()
        .any(|item_id| counts.get(item_id).copied() != Some(1))
});
```

Keep `render_materialization_lines` deletion behavior for records whose final item was intentionally retired; the fix must distinguish an untouched empty legacy record from a changed record emptied by retirement.

- [ ] **Step 3: Preserve a JSONL record boundary before append**

After opening the file under the existing shared lock and before writing the serialized new record, insert exactly one newline only when required:

```rust
if !raw.is_empty() && !raw.ends_with('\n') {
    writeln!(file)?;
}
writeln!(file, "{line}")?;
```

Do not rewrite or normalize existing bytes, including malformed content and CRLF endings.

- [ ] **Step 4: Run the focused tests and prove GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-core prefixless_user_symlink_keeps_its_referenced_generation_alive -- --nocapture
scripts/spur-cargo test -p spur-core hint_snapshot_preserves_empty_legacy_record_and_malformed_lines -- --nocapture
scripts/spur-cargo test -p spur-core append_separates_an_existing_record_without_final_newline -- --nocapture
scripts/spur-cargo test -p spur-core skills::projection::reconcile::tests -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
```

Expected: every command exits 0; the existing 33 reconciliation tests and 10 materialization tests remain green in addition to the new regressions.

- [ ] **Step 5: Commit the minimal implementation**

```bash
git add crates/spur-core/src/skills/projection/reconcile.rs crates/spur-core/src/explore/materialize.rs
git commit -m "fix(skills): rsp-4rf preserve final ownership edges"
```

### Task 3: Run the complete recovery gate

**Files:**
- Verify only; no new files.

- [ ] **Step 1: Verify exact scope and TDD ancestry**

```bash
git status --short
git diff --name-only HEAD~2..HEAD
git log --oneline --reverse HEAD~2..HEAD
git diff --check HEAD~2..HEAD
```

Expected: clean worktree; only the two locked Rust files changed; failing-test commit precedes the fix commit; diff check exits 0.

- [ ] **Step 2: Run the complete projection suites**

```bash
scripts/spur-cargo test -p spur-core skills::projection -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
```

Expected: all projection and materialization tests pass with no ignored new regression.

- [ ] **Step 3: Run formatting and lint gates**

```bash
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core --lib -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 4: Record completion without expanding scope**

Confirm the worker worktree is clean, summarize the three ownership/compatibility invariants, and emit the canonical SPUR completion audit. If any fix requires files outside the two-file allowlist, emit a scope-drift signal instead of editing them.

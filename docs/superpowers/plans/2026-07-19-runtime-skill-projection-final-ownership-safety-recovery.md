# Runtime Skill Projection Final Ownership-Safety Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Follow the repository SPUR transaction, plan-task, TDD, and verification skills throughout.

**Goal:** Close the final two fail-soft ownership gaps so a manifest-owned self-loop and an unavailable legacy-hint snapshot can never abort startup or leave a prefixless directory projection dangling after generation garbage collection.

**Architecture:** Continue from the preserved Attempt 3 implementation. Reuse its existing fail-soft generation-retention helper at the remaining prior-manifest seam, then make exclude-based retention understand the two adapter target shapes: Cursor excludes the projected file itself, while every directory adapter historically excluded `<legacy-target>/SKILL.md` and therefore needs that exact legacy parent considered as a second candidate. Keep retention conservative, exact, adapter-bounded, and separate from ownership adoption.

**Tech Stack:** Rust 2021, Tokio tests, Unix symlink fixtures, `spur_worktree::WorktreeManager`, and the workspace `scripts/spur-cargo` wrapper.

---

## Locked base, scope, and invariants

- Base this work on the committed planning branch descended from preserved candidate head `6dea8aed08f42de0bbdc88e0f559213028be4265`.
- The rejected predecessor was plan `50c42dea-3cfc-481a-b5b5-4c59c829fa6d`, task `rsp-4rf-final-recovery`, issue `bd-3lsiu`. This is a new recovery task, not a retry of that exhausted task.
- The authoritative behavior remains `docs/superpowers/specs/2026-07-17-runtime-skill-projection-design.md`, especially:
  - ownership loss is a warning and must not block startup;
  - changed targets are preserved and ownership is relinquished;
  - generation GC never deletes a generation referenced by a preserved target;
  - arbitrary paths and links are never adopted merely because they match a naming convention.
- Modify only `crates/spur-core/src/skills/projection/reconcile.rs`.
- Keep all public APIs, manifest schemas, adapter renderers, installer marker rules, JSONL formats, dependency sets, and launch integration unchanged.
- Do not scan an adapter root. Candidate discovery must start only from exact persisted worktree-exclude entries.
- Do not weaken ownership classification. Retaining a referenced generation is not ownership proof and must not adopt or remove a target.
- Treat an exact candidate that cannot be inspected or resolved as ownership uncertainty: warn and retain every existing generation for that adapter.
- Preserve the selective-GC invariant: an unrelated unreferenced generation must still be removed.
- Use exactly two worker commits:
  1. a RED regression commit;
  2. the minimal implementation/fix commit.
- Run every Rust build, test, format, and lint command through `scripts/spur-cargo`; never invoke bare `cargo`.

## Root-cause map

1. `retain_preserved_generation` handles a prior manifest symlink by propagating the result of `generation_reference_for_target` directly. A self-loop makes `canonicalize` return `ELOOP`, so an expected `OwnershipLost` warning becomes a fatal reconciliation error.
2. When `legacy_materialization_hints` cannot lock or read its JSONL file, the reconciler correctly receives no ownership hints and must not adopt/remove any prefixless target. GC still has the persisted exact exclude. For Cursor that exclude is the target file, but for seven directory adapters it is `<legacy-directory>/SKILL.md`; inspecting only that final regular file misses the parent directory symlink and allows its old generation to be collected.

### Task 1: Add the final ownership-uncertainty regressions

**Files:**
- Modify tests in `crates/spur-core/src/skills/projection/reconcile.rs`

- [ ] **Step 1: Add a manifest-owned self-loop regression**

Add this Unix Tokio test beside the current ownership-loss and GC tests:

```rust
#[cfg(unix)]
#[tokio::test]
async fn manifest_owned_self_loop_is_relinquished_without_blocking_later_sweeps() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_bundled_skill("looped", "both", "OLD");
    let first = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();
    let old_generation = generation_root(&fixture, &first.generation);
    let target = codex_target(&fixture, "looped");

    std::fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink("spurpower-looped", &target).unwrap();
    remove_source(&fixture, "looped");

    let second = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();

    assert!(second.skipped.iter().any(|skip| {
        skip.skill_id == "looped" && skip.reason == ProjectionSkipReason::OwnershipLost
    }));
    assert_eq!(
        std::fs::read_link(&target).unwrap(),
        Path::new("spurpower-looped")
    );
    assert!(old_generation.exists());

    fixture.write_bundled_skill("fresh-after-loop", "both", "FRESH");
    reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_link(&target).unwrap(),
        Path::new("spurpower-looped")
    );
    assert!(old_generation.exists());
}
```

This must exercise the stale prior-manifest path: first reconcile creates ownership, the target is replaced by a self-loop, the source is removed, the next reconcile reports `OwnershipLost`, and a subsequent generation sweep remains non-blocking.

- [ ] **Step 2: Add a production-shaped unavailable-hint regression**

Add this Unix Tokio test near `prefixless_user_symlink_keeps_its_referenced_generation_alive`:

```rust
#[cfg(unix)]
#[tokio::test]
async fn unavailable_hint_snapshot_preserves_prefixless_directory_generation_across_sweeps() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_pool_skill("legacy-exclude", "clean", "OLD");
    let first = publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
    let old_generation = first.root.clone();
    let target =
        legacy_target(Adapter::Codex, fixture.launch_root(), "legacy-exclude").unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    NativeLinker
        .symlink(
            &relative_symlink_source(
                &old_generation.join("skills/legacy-exclude"),
                &target,
            )
            .unwrap(),
            &target,
            TargetKind::Directory,
        )
        .unwrap();
    let original_destination = std::fs::read_link(&target).unwrap();
    let legacy_exclude =
        normalized_relative(fixture.launch_root(), &target.join("SKILL.md")).unwrap();
    fixture
        .worktrees()
        .add_worktree_excludes(
            fixture.launch_root(),
            std::slice::from_ref(&legacy_exclude),
        )
        .await
        .unwrap();

    let cache = fixture.repo_root().join(".spur/explore/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("materializations.jsonl"), b"\xff").unwrap();
    assert!(snapshot_legacy_materialization_hints(
        fixture.repo_root(),
        fixture.launch_root(),
    )
    .is_empty());

    fixture.write_pool_skill("legacy-exclude", "clean", "NEW");
    let second = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();
    assert_ne!(first.digest, second.generation);
    assert_eq!(std::fs::read_link(&target).unwrap(), original_destination);
    assert!(old_generation.exists());
    assert!(std::fs::canonicalize(&target)
        .unwrap()
        .starts_with(&old_generation));

    fixture.write_bundled_skill("fresh-after-hint-failure", "both", "FRESH");
    let third = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
        .await
        .unwrap();

    assert_ne!(second.generation, third.generation);
    assert_eq!(std::fs::read_link(&target).unwrap(), original_destination);
    assert!(old_generation.exists());
    assert!(std::fs::canonicalize(&target)
        .unwrap()
        .starts_with(&old_generation));
    assert!(!generation_root(&fixture, &second.generation).exists());
}
```

The invalid UTF-8 makes `read_to_string` fail deterministically on every platform without relying on Unix permission behavior. The exclude is the exact path produced by the legacy materializer for a directory adapter. The final assertion proves the fix preserves the referenced old generation without disabling selective GC.

- [ ] **Step 3: Prove the two behavioral regressions are RED**

Before introducing a reference to the new private helper, run:

```bash
scripts/spur-cargo test -p spur-core manifest_owned_self_loop_is_relinquished_without_blocking_later_sweeps -- --nocapture
scripts/spur-cargo test -p spur-core unavailable_hint_snapshot_preserves_prefixless_directory_generation_across_sweeps -- --nocapture
```

Expected before implementation:

- The self-loop test returns a reconciliation error containing `resolve preserved target` / `Too many levels of symbolic links`.
- The unavailable-hint test removes `old_generation`, causing the existence or canonicalization assertion to fail.

- [ ] **Step 4: Add a table test for all adapter target shapes and prove compile RED**

Introduce the private candidate helper name used by Task 2 in the test expectation and cover every adapter:

```rust
#[test]
fn preserved_exclude_candidates_follow_every_adapter_target_shape() {
    let launch = tempfile::tempdir().unwrap();
    let payload = crate::skills::SkillPayload {
        id: "shape".into(),
        description: "shape".into(),
        body: "BODY".into(),
        source: crate::skills::SkillSource::Pool,
        role: crate::skills::SkillRole::Both,
    };

    for &adapter in Adapter::all() {
        let rendered = adapter.render_with_prefix(&payload, launch.path(), "");
        let relative = normalized_relative(launch.path(), &rendered.path).unwrap();
        let candidates =
            preserved_exclude_candidates(launch.path(), adapter, &relative).unwrap();
        let mut expected = vec![rendered.path.clone()];
        if adapter.target_is_directory() {
            expected.push(rendered.path.parent().unwrap().to_path_buf());
        }
        assert_eq!(candidates, expected, "adapter={}", adapter.key());
    }
}
```

This locks the required shape distinction: the seven directory adapters get the exact file plus its validated legacy directory parent; Cursor gets only its exact `.mdc` file.

Run:

```bash
scripts/spur-cargo test -p spur-core preserved_exclude_candidates_follow_every_adapter_target_shape -- --nocapture
```

Expected: the target-shape test does not compile because `preserved_exclude_candidates` does not exist yet. This compile failure is an intentional final RED boundary after both behavioral failures have already been recorded.

- [ ] **Step 5: Commit only the failing regressions**

```bash
git add crates/spur-core/src/skills/projection/reconcile.rs
git commit -m "test(skills): rsp-4rf2 cover final ownership uncertainty"
```

### Task 2: Make retained-generation discovery fail-soft and adapter-aware

**Files:**
- Modify `crates/spur-core/src/skills/projection/reconcile.rs`

- [ ] **Step 1: Reuse the fail-soft helper for prior manifest symlinks**

Replace the direct fallible resolution in `retain_preserved_generation`:

```rust
fn retain_preserved_generation(
    projection_root: &Path,
    prior: &ProjectionManifest,
    prior_record: &TargetRecord,
    target: &Path,
    preserved: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    match prior_record.mode {
        ProjectionMode::Copy => {
            preserved.insert(prior.generation.clone());
        }
        ProjectionMode::Symlink => {
            retain_generation_reference_for_target(projection_root, target, preserved)?;
        }
    }
    Ok(())
}
```

Do not special-case `ELOOP`. The existing `retain_generation_reference_for_target` already implements the locked conservative policy for every resolution error: warn and call `retain_all_generation_references`.

- [ ] **Step 2: Derive only adapter-valid candidates from exact excludes**

Add this private helper next to `preserved_generation_references`:

```rust
fn preserved_exclude_candidates(
    launch_root: &Path,
    adapter: crate::skills::adapters::Adapter,
    relative: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let exact = launch_root.join(relative);
    let mut candidates = vec![exact.clone()];
    if !adapter.target_is_directory()
        || exact.file_name().is_none_or(|name| name != "SKILL.md")
    {
        return Ok(candidates);
    }
    let Some(parent) = exact.parent() else {
        return Ok(candidates);
    };
    let Some(skill_id) = parent.file_name().and_then(|name| name.to_str()) else {
        return Ok(candidates);
    };
    if legacy_target(adapter, launch_root, skill_id)? == parent {
        candidates.push(parent.to_path_buf());
    }
    Ok(candidates)
}
```

The equality against `legacy_target` is the adapter boundary. It prevents an arbitrary excluded `SKILL.md` elsewhere in the worktree from promoting its parent into a projection candidate.

- [ ] **Step 3: Probe each exact candidate through the shared conservative helper**

Thread `request.adapter` into `preserved_generation_references` at its single call site:

```rust
preserved_generations.extend(
    preserved_generation_references(
        request.launch_root,
        &projection_root,
        request.adapter,
        &excluded,
        &next,
    )
    .map_err(|error| {
        projection_error(
            request.clone(),
            ProjectionPhase::GarbageCollect,
            None,
            error,
        )
    })?,
);
```

Update the helper body so both the exact exclude and any adapter-validated parent candidate are checked:

```rust
fn preserved_generation_references(
    launch_root: &Path,
    projection_root: &Path,
    adapter: crate::skills::adapters::Adapter,
    excluded: &[String],
    manifest: &ProjectionManifest,
) -> anyhow::Result<BTreeSet<String>> {
    let managed = manifest
        .targets
        .iter()
        .map(|target| target.target_rel.as_str())
        .collect::<HashSet<_>>();
    let mut retained = BTreeSet::new();
    for relative in excluded {
        for target in preserved_exclude_candidates(launch_root, adapter, relative)? {
            let target_rel = normalized_relative(launch_root, &target)?;
            if managed.contains(target_rel.as_str()) {
                continue;
            }
            retain_generation_reference_for_target(
                projection_root,
                &target,
                &mut retained,
            )?;
        }
    }
    Ok(retained)
}
```

Do not canonicalize while deriving candidates and do not enumerate target roots. `generation_reference_for_target` remains the sole referent classifier, and all of its errors remain fail-soft through `retain_generation_reference_for_target`.

- [ ] **Step 4: Prove the focused tests are GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-core manifest_owned_self_loop_is_relinquished_without_blocking_later_sweeps -- --nocapture
scripts/spur-cargo test -p spur-core unavailable_hint_snapshot_preserves_prefixless_directory_generation_across_sweeps -- --nocapture
scripts/spur-cargo test -p spur-core preserved_exclude_candidates_follow_every_adapter_target_shape -- --nocapture
scripts/spur-cargo test -p spur-core unrelated_self_loop_symlink_does_not_block_reconciliation -- --nocapture
scripts/spur-cargo test -p spur-core prefixless_user_symlink_keeps_its_referenced_generation_alive -- --nocapture
scripts/spur-cargo test -p spur-core garbage_collection_removes_only_unreferenced_generations -- --nocapture
```

Expected: every command exits zero. The two multi-sweep regressions preserve their links and referenced generation; the existing selective-GC test still removes an unrelated generation.

- [ ] **Step 5: Commit the minimal implementation**

```bash
git add crates/spur-core/src/skills/projection/reconcile.rs
git commit -m "fix(skills): rsp-4rf2 preserve uncertain ownership"
```

### Task 3: Run completion gates and record evidence

**Files:**
- No source changes expected.

- [ ] **Step 1: Verify the complete projection and materialization suites**

```bash
scripts/spur-cargo test -p spur-core skills::projection::reconcile::tests -- --nocapture
scripts/spur-cargo test -p spur-core skills::projection -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
```

- [ ] **Step 2: Verify formatting and lint**

```bash
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core --lib -- -D warnings
```

- [ ] **Step 3: Verify scope, commit cadence, and patch hygiene**

Set `RECOVERY_BASE` to the committed plan head supplied by the brain, then run:

```bash
git status --short
git log --oneline "${RECOVERY_BASE}..HEAD"
git diff --name-only "${RECOVERY_BASE}..HEAD"
git diff --check "${RECOVERY_BASE}..HEAD"
```

Expected:

- the worktree is clean;
- the worker range contains exactly the RED `test(skills)` commit followed by the GREEN `fix(skills)` commit;
- `git diff --name-only` prints only `crates/spur-core/src/skills/projection/reconcile.rs`;
- `git diff --check` exits zero.

- [ ] **Step 4: Record completion evidence**

In the beads task comment, record:

- the three focused regression results;
- projection, reconciliation, and materialization suite counts;
- format and clippy results;
- the two worker commit hashes;
- the exact one-file scope output;
- confirmation that no public API, manifest, adapter renderer, marker rule, JSONL format, or dependency changed.

Do not claim completion if any gate is skipped or failing.

# Runtime Skill Projection Reconciler Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four remaining Task 4 review gaps without changing the locked projection API or starting downstream CLI/launch integration.

**Architecture:** Keep reconciliation adapter-local, but move legacy-record retirement to a post-sweep decision that knows which adapters were examined. Extend preflight ownership evidence with one Git-index snapshot and retain generation references found through every preserved symlink. Preserve typed skill context by wrapping per-skill generation failures while leaving adapter-wide failures unscoped.

**Tech Stack:** Rust 2021, Tokio, `fs4`, `tempfile`, Git CLI, existing `spur-core` projection fixtures, and `scripts/spur-cargo`.

---

## Source of Truth and Scope

This is a focused recovery from Task 4 attempt 3 on commit
`621e8ca94b74ca4bb26824eba5a1209222491222`. The locked contracts remain:

- `docs/superpowers/specs/2026-07-17-runtime-skill-projection-design.md`
- `docs/superpowers/plans/2026-07-17-runtime-skill-projection.md`, Task 4
- rejection audit for delegation `32fbe605cdbd4526`

Do not change public `reconcile`, `reconcile_with_worktrees`, or
`reconcile_many` signatures. Do not touch CLI, worker launch, brain launch, or
acceptance-test files. Do not add dependencies.

## File Map

| File | Responsibility in this recovery |
|---|---|
| `crates/spur-core/src/skills/projection/reconcile.rs` | Regressions, deferred legacy retirement, Git-index ownership preflight, preserved-generation retention, and typed error mapping |
| `crates/spur-core/src/skills/projection/mod.rs` | Make `reconcile_many` run an all-adapter sweep before legacy hints are retired |
| `crates/spur-core/src/explore/materialize.rs` | Serialize append/retire mutations to `materializations.jsonl` with one lock |
| `crates/spur-core/src/skills/projection/generation.rs` | Attach a canonical skill ID to failures raised while staging one selected skill |

---

### Task 1: Repair the ownership-safe reconciler

**Files:**

- Modify: `crates/spur-core/src/skills/projection/reconcile.rs`
- Modify: `crates/spur-core/src/skills/projection/mod.rs`
- Modify: `crates/spur-core/src/explore/materialize.rs`
- Modify: `crates/spur-core/src/skills/projection/generation.rs`
- Test: co-located tests in `reconcile.rs` and `materialize.rs`

- [ ] **Step 1: Add five failing regressions**

Add these cases to `projection::reconcile::tests`, using the existing
`ProjectionFixture`, `NativeLinker`, `publish_generation`,
`relative_symlink_source`, and materialization-record helpers.

```rust
#[tokio::test]
async fn reconcile_many_keeps_legacy_hint_until_owning_adapter_runs() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_pool_skill("cross-adapter", "clean", "POOL BODY");
    let skill = fixture
        .resolve()
        .unwrap()
        .into_iter()
        .find(|skill| skill.payload.id == "cross-adapter")
        .unwrap();
    let rendered = Adapter::Codex.render_with_prefix(
        &skill.payload,
        fixture.launch_root(),
        "",
    );
    let legacy_target = rendered.path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&legacy_target).unwrap();
    std::fs::write(&rendered.path, rendered.bytes).unwrap();
    append_materialization_record(
        fixture.repo_root(),
        &MaterializationRecord {
            recorded_at_epoch: 1,
            delegation_id: "cross-adapter".into(),
            agent: "codex".into(),
            worktree: fixture.launch_root().display().to_string(),
            items: vec!["cross-adapter".into()],
        },
    )
    .unwrap();

    let summaries = reconcile_many(
        fixture.repo_root(),
        fixture.launch_root(),
        &[Adapter::Cursor, Adapter::Codex],
    )
    .await
    .unwrap();

    assert_eq!(summaries.len(), 2);
    assert!(!legacy_target.exists());
    assert!(codex_target(&fixture, "cross-adapter").exists());
    assert!(read_recent_materializations(fixture.repo_root(), 10)
        .iter()
        .all(|record| !record.items.iter().any(|item| item == "cross-adapter")));
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_user_symlink_keeps_its_referenced_generation_alive() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_bundled_skill("collision", "both", "COLLISION");
    fixture.write_bundled_skill("source", "both", "OLD");
    let first = publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
    let target = codex_target(&fixture, "collision");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let source = first.root.join("skills/source");
    NativeLinker
        .symlink(
            &relative_symlink_source(&source, &target).unwrap(),
            &target,
            TargetKind::Directory,
        )
        .unwrap();
    fixture.write_bundled_skill("source", "both", "NEW");

    let summary = reconcile_with_linker(
        fixture.worktrees(),
        fixture.request(),
        &NativeLinker,
    )
    .await
    .unwrap();

    assert_eq!(summary.skipped[0].reason, ProjectionSkipReason::UserOwned);
    assert!(target.exists());
    assert!(first.root.exists());
    assert!(std::fs::canonicalize(&target).unwrap().starts_with(&first.root));
}

#[tokio::test]
async fn tracked_but_absent_target_is_preserved_as_user_owned() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_bundled_skill("tracked-absent", "both", "GENERATED");
    let target = codex_target(&fixture, "tracked-absent");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("SKILL.md"), b"USER TRACKED\n").unwrap();
    run_git(fixture.launch_root(), &["add", ".codex/skills/spurpower-tracked-absent/SKILL.md"]);
    run_git(fixture.launch_root(), &["commit", "--quiet", "-m", "track collision"]);
    std::fs::remove_dir_all(&target).unwrap();

    let summary = reconcile_with_linker(
        fixture.worktrees(),
        fixture.request(),
        &NativeLinker,
    )
    .await
    .unwrap();

    assert!(!path_exists_no_follow(&target).unwrap());
    assert_eq!(summary.skipped.len(), 1);
    assert_eq!(summary.skipped[0].skill_id, "tracked-absent");
    assert_eq!(summary.skipped[0].reason, ProjectionSkipReason::UserOwned);
    assert!(read_manifest(&fixture).targets.is_empty());
}

#[test]
fn typed_projection_errors_keep_available_skill_ids() {
    let invalid = resolver::ResolveError::InvalidId(crate::skills::InvalidSkillId {
        id: "Bad-Id".into(),
        reason: "uppercase",
    });
    assert_eq!(resolve_error_skill_id(&invalid).as_deref(), Some("Bad-Id"));

    let catalog = resolver::ResolveError::Catalog(
        crate::skills::SkillCatalogError::ReadSkill {
            id: "catalog-skill".into(),
            path: PathBuf::from("assets/skills/catalog-skill/SKILL.md"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        },
    );
    assert_eq!(resolve_error_skill_id(&catalog).as_deref(), Some("catalog-skill"));

    let generated = generation::GenerationError::Skill {
        skill_id: "stage-skill".into(),
        source: Box::new(generation::GenerationError::Io {
            path: PathBuf::from("asset.bin"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        }),
    };
    assert_eq!(generation_error_skill_id(&generated).as_deref(), Some("stage-skill"));
}
```

Add this test helper beside `git_status`:

```rust
fn run_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
```

Add a deterministic mutation-lock regression to `explore::materialize::tests`.
It must prove both writers use the same lock instead of relying on a timing-only
lost-update race:

```rust
#[test]
fn append_and_retire_share_the_materialization_record_lock() {
    use std::sync::mpsc;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let worktree = repo.path().join("worker");
    let record = |delegation: &str, item: &str| MaterializationRecord {
        recorded_at_epoch: 1,
        delegation_id: delegation.into(),
        agent: "codex".into(),
        worktree: worktree.display().to_string(),
        items: vec![item.into()],
    };
    append_materialization_record(repo.path(), &record("old", "old-skill")).unwrap();

    let cache = repo.path().join(".spur/explore/cache");
    let guard = lock_materialization_records(&cache).unwrap();
    let (tx, rx) = mpsc::channel();
    let repo_path = repo.path().to_path_buf();
    let appended = record("new", "new-skill");
    std::thread::spawn(move || {
        tx.send(append_materialization_record(&repo_path, &appended))
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(guard);
    rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

    let guard = lock_materialization_records(&cache).unwrap();
    let (tx, rx) = mpsc::channel();
    let repo_path = repo.path().to_path_buf();
    let worktree_path = worktree.clone();
    std::thread::spawn(move || {
        tx.send(retire_legacy_materializations(
            &repo_path,
            &worktree_path,
            &["old-skill".into()],
        ))
        .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(guard);
    rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

    let records = read_recent_materializations(repo.path(), 10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].items, vec!["new-skill"]);
}
```

- [ ] **Step 2: Run the new tests and prove the red state**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::reconcile::tests -- --nocapture
```

Expected: FAIL because the Cursor pass consumes the Codex migration hint, the
shared mutation-lock helper does not exist, GC removes the first generation,
the absent tracked target is installed, and the new typed generation variant
does not exist.

- [ ] **Step 3: Commit only the failing regressions**

```bash
git add \
  crates/spur-core/src/explore/materialize.rs \
  crates/spur-core/src/skills/projection/reconcile.rs
git commit -m "test(skills): rsp-4r cover reconciler recovery gaps"
```

- [ ] **Step 4: Defer and serialize legacy-record retirement**

Refactor the private reconcile path so one adapter returns both its
`ProjectionSummary` and the legacy IDs it examined. `reconcile` and
`reconcile_with_worktrees` may retire after one adapter only when no existing
prefixless target belongs to an unexamined adapter. `reconcile_many` must run
every requested adapter successfully first, then perform exactly one retirement
decision using the complete requested-adapter set.

Use a helper with this semantic contract; names may remain private:

```rust
fn legacy_ids_ready_to_retire(
    launch_root: &Path,
    examined: &[Adapter],
    skill_ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let examined = examined.iter().copied().collect::<HashSet<_>>();
    skill_ids
        .iter()
        .filter_map(|skill_id| {
            let has_unexamined_target = Adapter::all().iter().any(|adapter| {
                !examined.contains(adapter)
                    && legacy_target(adapter, launch_root, skill_id)
                        .is_some_and(|target| path_exists_no_follow(&target).unwrap_or(true))
            });
            (!has_unexamined_target).then(|| skill_id.clone())
        })
        .collect::<Vec<_>>()
        .pipe(Ok)
}
```

Do not use `unwrap_or(true)` or `.pipe()` literally if they obscure error
propagation; the implementation must propagate inspection errors. An existing
target under an examined adapter is already classified as migrated,
user-edited, or user-owned, so it does not keep the one-time hint alive.

In `materialize.rs`, acquire one sibling lock file before both append and
read-modify-write retirement:

```rust
fn lock_materialization_records(cache_dir: &Path) -> anyhow::Result<std::fs::File> {
    use fs4::fs_std::FileExt as _;
    std::fs::create_dir_all(cache_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(cache_dir.join("materializations.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}
```

Hold that guard through the append, or through retirement's read, temporary
write, `sync_all`, and `persist`. Retirement must re-read the file while locked
so a concurrent append cannot be lost. Preserve malformed JSONL lines exactly
as the current code does.

- [ ] **Step 5: Retain every preserved symlink generation**

Add a helper independent of prior-manifest ownership:

```rust
fn retain_generation_reference_for_target(
    projection_root: &Path,
    target: &Path,
    preserved: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    if let Some(generation) = generation_reference_for_target(projection_root, target)? {
        preserved.insert(generation);
    }
    Ok(())
}
```

Call it in both no-prior collision branches (`UserEdited` and `UserOwned`)
before returning the skip. Keep the existing historical-exclude scan as a
second safety net; do not weaken manifest/pending retention.

- [ ] **Step 6: Treat an absent Git-index path as user-owned**

Read the launch root's index once before desired-target planning:

```rust
fn git_tracked_paths(launch_root: &Path) -> anyhow::Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "--cached"])
        .current_dir(launch_root)
        .output()
        .context("list tracked projection targets")?;
    anyhow::ensure!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
        .collect())
}

fn target_is_tracked(target_rel: &str, tracked: &[String]) -> bool {
    tracked.iter().any(|path| {
        path == target_rel
            || path
                .strip_prefix(target_rel)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}
```

Pass this snapshot into `plan_desired_target`. Only the `None` + absent branch
lacks manifest, pending, or marker ownership proof. If the target itself or any
descendant is tracked, leave it absent, append one `UserOwned` skip, and omit it
from the next manifest. Prior-manifest targets remain recoverable even when Git
tracks them, because the manifest is explicit ownership proof.

- [ ] **Step 7: Preserve typed skill context**

Add a private-source variant to `GenerationError`:

```rust
#[error("generation staging failed for skill {skill_id}: {source}")]
Skill {
    skill_id: String,
    #[source]
    source: Box<GenerationError>,
},
```

Wrap `stage_skill` failures in `publish_generation`, except that an existing
`UnsafeSourcePath` already carries the same ID and may remain unwrapped. Map
`ResolveError::InvalidId`, `SkillCatalogError::ReadSkill`, and
`SkillCatalogError::InvalidSkillId`, plus the new generation `Skill` variant,
in the existing ID extractors. Adapter-wide generation failures continue with
`skill_id: None` because no canonical skill is available.

- [ ] **Step 8: Run focused verification**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::reconcile::tests -- --nocapture
scripts/spur-cargo test -p spur-core skills::projection -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core --lib -- -D warnings
git diff --check
```

Expected: all commands exit 0. The focused suites include all five new
regressions and all 43 attempt-3 projection tests.

- [ ] **Step 9: Commit the recovery implementation**

```bash
git add \
  crates/spur-core/src/explore/materialize.rs \
  crates/spur-core/src/skills/projection/generation.rs \
  crates/spur-core/src/skills/projection/mod.rs \
  crates/spur-core/src/skills/projection/reconcile.rs
git commit -m "fix(skills): rsp-4r preserve projection ownership"
```

The worker must finish with a clean worktree and must not squash the red-test
and implementation commits itself. SPUR normalization may squash afterward.

---

## Recovery Gate

Do not begin Tasks 5–8 from the original plan until this recovery task is
independently reviewed and approved. Approval requires all five regressions,
the complete focused suites, formatting, remote clippy, a clean diff, and a
canonical beads completion audit.

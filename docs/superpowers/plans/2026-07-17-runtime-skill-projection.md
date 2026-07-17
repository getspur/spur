# Runtime Skill Projection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every bundled and active pool skill available through persistent, safely reconciled adapter projections before `spur skills init`, brain startup, or worker startup completes.

**Architecture:** Add a `skills::projection` subsystem with four narrow layers: effective-set resolution, immutable generation building, manifest-backed reconciliation, and launch integration. Existing adapter renderers remain the only format implementation; legacy marker logic is retained solely for safe ownership migration. CLI, brain, and worker paths call the same async projection API.

**Tech Stack:** Rust 2021, Tokio, Serde/JSON, SHA-256, `fs4` file locks, `tempfile`, existing `spur-worktree` Git-exclude support, existing SPUR adapter and Explore pool APIs.

---

## Source Documents and Required Discipline

- Design: `docs/superpowers/specs/2026-07-17-runtime-skill-projection-design.md`
- Existing installer contract: `docs/superpowers/specs/2026-04-19-skills-installer-design.md`
- Existing pool contract: `docs/superpowers/specs/2026-07-07-explore-command-design.md`
- Repository rules: `AGENTS.md`

For every assigned plan task:

1. Read the task's beads issue and verify its dependencies are approved.
2. Use `spurpower-plan-task-discipline`, `spurpower-test-driven-development`,
   and `spurpower-verification-before-completion`.
3. Modify only the files listed for that task. Emit a `scope_drift` signal
   before crossing a file boundary owned by another task.
4. Run compile-heavy commands only through `scripts/spur-cargo`.
5. Commit the failing test first, then commit the implementation that makes it
   pass. Do not merge or close the beads issue from the worker.

## File and Responsibility Map

| File | Responsibility |
|---|---|
| `crates/spur-core/src/skills/projection/mod.rs` | Public request, role/policy, summary, error, and single/multi-adapter entry points |
| `crates/spur-core/src/skills/projection/resolver.rs` | Deterministic bundled/pool/repository candidate loading, eligibility, precedence, and source verification |
| `crates/spur-core/src/skills/projection/generation.rs` | Adapter rendering, supporting-asset staging, generation hashing, validation, and immutable publication |
| `crates/spur-core/src/skills/projection/manifest.rs` | Versioned manifest, pending transaction, target/source/mode records, and atomic JSON I/O |
| `crates/spur-core/src/skills/projection/reconcile.rs` | Locking, ownership classification, migration, link/copy switching, rollback/recovery, excludes, and garbage collection |
| `crates/spur-core/src/skills/mod.rs` | Existing bundled catalog plus source identity needed by projection |
| `crates/spur-core/src/skills/adapters.rs` | Existing renderers plus stable adapter key/kind/target helpers |
| `crates/spur-core/src/skills/installer.rs` | Legacy marker validation and direct-installer compatibility; no new rendering path |
| `crates/spur-core/src/explore/materialize.rs` | Pool source loading and legacy materialization-record migration only |
| `crates/spur-cli/src/commands/init.rs` | Async manual projection and summary display |
| `crates/spur-cli/src/main.rs` | Await `skills init` |
| `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs` | Fatal pre-session worker projection hook |
| `crates/spur-core/src/orchestrator/connection.rs` | Fatal pre-connection brain projection hook shared by all brain connection callers |
| `crates/spur-core/tests/runtime_skill_projection.rs` | Cross-source acceptance and reconciliation behavior |
| `crates/spur-cli/tests/skills_init_projection.rs` | CLI initialization acceptance |

The projection store is always rooted at:

```text
<launch-root>/.spur/runtime/skill-projections/<adapter>/
  reconcile.lock
  manifest.json
  pending.json
  generations/<sha256>/<rendered-targets>
```

## Locked Public Contracts

Later tasks must use these names and fields exactly unless the worker emits a
`risk` signal and the brain approves an interface change:

```rust
// crates/spur-core/src/skills/projection/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    AllActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Brain,
    Worker,
    Init,
}

#[derive(Debug, Clone)]
pub struct ProjectionRequest<'a> {
    pub source_repo_root: &'a std::path::Path,
    pub launch_root: &'a std::path::Path,
    pub adapter: crate::skills::adapters::Adapter,
    pub role: RuntimeRole,
    pub policy: SelectionPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionSummary {
    pub adapter: String,
    pub generation: String,
    pub linked: Vec<std::path::PathBuf>,
    pub copied: Vec<std::path::PathBuf>,
    pub unchanged: Vec<std::path::PathBuf>,
    pub removed: Vec<std::path::PathBuf>,
    pub migrated: Vec<std::path::PathBuf>,
    pub skipped: Vec<ProjectionSkip>,
    pub selected: Vec<SelectedSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionSkipReason {
    UserOwned,
    UserEdited,
    OwnershipLost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSkip {
    pub skill_id: String,
    pub path: std::path::PathBuf,
    pub reason: ProjectionSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSource {
    pub skill_id: String,
    pub kind: resolver::ResolvedSourceKind,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPhase {
    Resolve,
    Generate,
    Manifest,
    Recover,
    Reconcile,
    Excludes,
    GarbageCollect,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "skill projection {phase:?} failed for {adapter} at {launch_root}: {source}"
)]
pub struct ProjectionError {
    pub phase: ProjectionPhase,
    pub launch_root: std::path::PathBuf,
    pub adapter: String,
    pub skill_id: Option<String>,
    #[source]
    pub source: anyhow::Error,
}

pub async fn reconcile(
    request: ProjectionRequest<'_>,
) -> Result<ProjectionSummary, ProjectionError>;

pub async fn reconcile_with_worktrees(
    worktrees: &spur_worktree::manager::WorktreeManager,
    request: ProjectionRequest<'_>,
) -> Result<ProjectionSummary, ProjectionError>;

pub async fn reconcile_many(
    source_repo_root: &std::path::Path,
    launch_root: &std::path::Path,
    adapters: &[crate::skills::adapters::Adapter],
) -> Result<Vec<ProjectionSummary>, ProjectionError>;
```

Declare `projection::resolver` as a public module (or publicly re-export
`ResolvedSourceKind`) because it appears in `SelectedSource`,
`TargetRecord`, and acceptance-test contracts. Keep all other projection
implementation modules private unless a later task explicitly requires a
public type from them.

`ProjectionError`, `ProjectionSkip`, and `SelectedSource` are public, typed,
and implement `Display` through `thiserror` or an explicit formatter. Error
messages include launch root, adapter, phase, and skill ID when available.

---

### Task 1: Make bundled role metadata non-filtering for adapter install

**Files:**
- Modify: `crates/spur-core/src/skills/installer.rs:253-337, 530-669`
- Modify: `crates/spur-core/src/skills/mod.rs:437-476`
- Modify: `crates/spur-core/tests/skills_installer.rs:212-251`

- [ ] **Step 1: Change the regression tests to require bundled brain skills on worker adapters**

Keep the existing repository-override test, rename it to
`run_keeps_brain_only_override_hermetic`, and add this bundled-source test:

```rust
#[test]
fn run_projects_bundled_brain_skill_to_worker_adapters() {
    let dir = tempfile::tempdir().unwrap();
    let skills = crate::skills::list_active_skills(dir.path()).unwrap();
    let bundled = skills
        .iter()
        .find(|skill| {
            skill.role == crate::skills::SkillRole::Brain
                && skill.source == crate::skills::SkillSource::Bundled
        })
        .expect("bundled brain skill");

    run(dir.path()).unwrap();

    let codex = dir
        .path()
        .join(".codex/skills")
        .join(format!("spurpower-{}", bundled.id))
        .join("SKILL.md");
    assert!(codex.exists(), "{} was not installed", codex.display());
}
```

Update `stale_managed_worker_adapter_file_for_brain_skill_is_removed` to
`managed_worker_adapter_file_for_bundled_brain_skill_is_retained` and assert
that the file exists after `run`. Update the two unit tests that expected an
empty bundled-brain adapter directory to be removed so they instead assert the
managed `SKILL.md` is created without deleting unrelated user files.

- [ ] **Step 2: Run the new regression test and confirm the old filter fails it**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::installer::tests::run_projects_bundled_brain_skill_to_worker_adapters -- --exact
```

Expected: FAIL because `run_filtered` removes or skips the Codex target.

- [ ] **Step 3: Commit the failing regression**

```bash
git add crates/spur-core/src/skills/installer.rs crates/spur-core/tests/skills_installer.rs
git commit -m "test(skills): rsp-1 require built-ins in adapters"
```

- [ ] **Step 4: Restrict role filtering to repository overrides**

Replace the filter in `run_filtered` with this exact source-aware condition:

```rust
use super::{SkillRole, SkillSource};

let hermetic_only = skill.source == SkillSource::Override
    && skill.role == SkillRole::Brain
    && *adapter != Adapter::SpurHermetic;
if hermetic_only {
    let rendered = adapter.render(skill, repo_root);
    remove_stale_managed_file(&rendered, &mut summary)?;
    continue;
}
```

Update `SkillRole` and `run_filtered` documentation: `role: brain` remains a
repository-override restriction, while `SkillSource::Bundled` is installable
for all selected adapters.

- [ ] **Step 5: Run focused installer tests**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::installer::tests -- --nocapture
scripts/spur-cargo test -p spur-core --test skills_installer -- --nocapture
```

Expected: PASS for both commands.

- [ ] **Step 6: Commit the policy fix**

```bash
git add crates/spur-core/src/skills/installer.rs crates/spur-core/src/skills/mod.rs crates/spur-core/tests/skills_installer.rs
git commit -m "fix(skills): rsp-1 install bundled brain skills"
```

---

### Task 2: Resolve the `AllActive` effective set with source precedence

**Depends on:** Task 1

**Files:**
- Create: `crates/spur-core/src/skills/projection/mod.rs`
- Create: `crates/spur-core/src/skills/projection/resolver.rs`
- Create: `crates/spur-core/src/skills/projection/test_support.rs`
- Modify: `crates/spur-core/src/skills/mod.rs:1-15, 438-544`
- Modify: `crates/spur-core/src/skills/adapters.rs:17-90`
- Modify: `crates/spur-core/src/explore/materialize.rs:198-260`

- [ ] **Step 1: Add failing resolver tests for precedence and eligibility**

Add tests under `skills::projection::resolver::tests` that create a temporary
bundled root through `<repo>/.spur/config.toml`, an accepted pool manifest, and
repository overrides. The central assertion is:

```rust
let resolved = resolve_effective_skills(
    repo.path(),
    Adapter::Codex,
    RuntimeRole::Worker,
    SelectionPolicy::AllActive,
)
.unwrap();

let by_id = resolved
    .iter()
    .map(|skill| (skill.payload.id.as_str(), skill.source.kind))
    .collect::<std::collections::BTreeMap<_, _>>();

assert_eq!(by_id["repo-wins"], ResolvedSourceKind::RepositoryOverride);
assert_eq!(by_id["pool-wins"], ResolvedSourceKind::Pool);
assert_eq!(by_id["bundled-only"], ResolvedSourceKind::Bundled);
assert_eq!(by_id["brain-builtin"], ResolvedSourceKind::Bundled);
```

Add a second test where a `role: brain` repository override shadows a bundled
ID. For `Adapter::Codex`, assert the repository candidate is ineligible and the
bundled candidate remains selected. Assert rejected pool verdicts and pool hash
mismatches return a typed error instead of entering the result.

- [ ] **Step 2: Run the resolver test and verify it fails to compile**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::resolver::tests -- --nocapture
```

Expected: FAIL because `skills::projection` and its resolver types do not exist.

- [ ] **Step 3: Commit the failing resolver contract**

```bash
git add crates/spur-core/src/skills/projection crates/spur-core/src/skills/mod.rs
git commit -m "test(skills): rsp-2 specify effective resolution"
```

- [ ] **Step 4: Add source identities and adapter helpers**

Extend the source enum without changing existing variant meaning:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Bundled,
    Pool,
    Override,
}
```

In `projection/mod.rs`, declare `pub mod resolver;` so the locked public
contracts do not expose a private type.

Add these adapter methods and move the current `adapter_for_kind` match into
`Adapter::for_agent_kind`:

```rust
pub fn key(self) -> &'static str;
pub fn for_agent_kind(kind: spur_acp::types::AgentKind) -> Option<Self>;
pub fn target_is_directory(self) -> bool {
    self != Self::Cursor
}
```

`Grok` and `Generic` map to `None`; the seven known external kinds keep their
current mapping. `SpurHermetic` is selected only by CLI configuration, not from
an `AgentKind`.

- [ ] **Step 5: Implement the resolver types and deterministic merge**

Define the resolver contract as:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedSourceKind {
    Bundled,
    Pool,
    RepositoryOverride,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedSource {
    pub kind: ResolvedSourceKind,
    pub content_sha256: String,
    pub source_dir: std::path::PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub payload: crate::skills::SkillPayload,
    pub source: ResolvedSource,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Catalog(#[from] crate::skills::SkillCatalogError),
    #[error(transparent)]
    InvalidId(#[from] crate::skills::InvalidSkillId),
    #[error("failed to load layered pool manifest: {0}")]
    Manifest(#[source] anyhow::Error),
    #[error(
        "pool skill {id} digest mismatch: expected {expected}, actual {actual}"
    )]
    PoolDigestMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error(
        "pool skill {id} collides with bundled without replaced-bundled verdict"
    )]
    PoolReplacementNotAuthorized { id: String },
    #[error("failed to read skill source {path}: {source}")]
    ReadSource {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn resolve_effective_skills(
    repo_root: &std::path::Path,
    adapter: crate::skills::adapters::Adapter,
    role: super::RuntimeRole,
    policy: super::SelectionPolicy,
) -> Result<Vec<ResolvedSkill>, ResolveError>;
```

Implement candidate loading in this order, then merge by ID in a
`BTreeMap`: bundled baseline, accepted pool replacement, eligible repository
override. Before insertion:

- validate every ID with `skills::validate_id`;
- compute/verify source directory hashes with `explore::content_hash`;
- require pool kind `Skill` and verdict `clean | overridden |
  replaced-bundled`;
- require `replaced-bundled` when a pool ID collides with bundled;
- treat bundled and pool skills as adapter-eligible regardless of their role;
- keep a `role: brain` repository override eligible only for
  `Adapter::SpurHermetic`.

Expose a crate-visible pool loader from `explore::materialize` that returns
`ResolvedSkill` inputs and delete the duplicate `adapter_for_kind` match after
all call sites use `Adapter::for_agent_kind`.

Add `#[cfg(test)] mod test_support;` with this reusable fixture contract for
Tasks 2-4:

```rust
pub struct ProjectionFixture {
    repo: tempfile::TempDir,
    assets: tempfile::TempDir,
    launch: tempfile::TempDir,
    adapter: crate::skills::adapters::Adapter,
    worktrees: spur_worktree::manager::WorktreeManager,
}

impl ProjectionFixture {
    pub fn new(adapter: crate::skills::adapters::Adapter) -> Self;
    pub fn repo_root(&self) -> &std::path::Path;
    pub fn launch_root(&self) -> &std::path::Path;
    pub fn worktrees(&self) -> &spur_worktree::manager::WorktreeManager;
    pub fn request(&self) -> super::ProjectionRequest<'_>;
    pub fn write_bundled_skill(&self, id: &str, role: &str, body: &str);
    pub fn write_repository_override(&self, id: &str, role: &str, body: &str);
    pub fn write_pool_skill(&self, id: &str, verdict: &str, body: &str);
    pub fn write_support(&self, id: &str, relative: &str, bytes: &[u8]);
    pub fn resolve(&self) -> Result<Vec<super::resolver::ResolvedSkill>, super::resolver::ResolveError>;
}
```

`new` initializes Git in both roots and writes `.spur/config.toml` with the
temporary absolute bundled directory. Pool helpers compute
`explore::content_hash` after writing and save a layered `Manifest`.

- [ ] **Step 6: Run resolver and existing catalog tests**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::resolver::tests -- --nocapture
scripts/spur-cargo test -p spur-core skills::tests -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
```

Expected: PASS. Existing global/local pool inheritance remains intact.

- [ ] **Step 7: Commit effective-set resolution**

```bash
git add crates/spur-core/src/skills crates/spur-core/src/explore/materialize.rs
git commit -m "feat(skills): rsp-2 resolve all active sources"
```

---

### Task 3: Build immutable adapter generations

**Depends on:** Task 2

**Files:**
- Create: `crates/spur-core/src/skills/projection/generation.rs`
- Modify: `crates/spur-core/src/skills/projection/mod.rs`
- Modify: `crates/spur-core/src/skills/adapters.rs:40-210`

- [ ] **Step 1: Add failing generation tests**

Cover deterministic reuse, adapter-native output, and supporting files:

```rust
#[test]
fn equal_inputs_reuse_the_same_generation() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_bundled_skill("with-assets", "both", "BODY");
    fixture.write_support("with-assets", "scripts/check.sh", b"#!/bin/sh\n");
    let selected = fixture.resolve().unwrap();

    let first = publish_generation(fixture.request(), &selected).unwrap();
    let second = publish_generation(fixture.request(), &selected).unwrap();

    assert_eq!(first.digest, second.digest);
    assert_eq!(first.root, second.root);
    assert!(first.root.join("skills/with-assets/scripts/check.sh").exists());
    assert!(first.root.join("skills/with-assets/SKILL.md").exists());
}
```

Add Cursor and Kiro cases asserting `.mdc` rendering and the Kiro steering
companion record. Add a source symlink escaping the skill root and assert a
typed `GenerationError::UnsafeSourcePath`.

- [ ] **Step 2: Run the generation tests and confirm the missing module fails**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::generation::tests -- --nocapture
```

Expected: FAIL because `publish_generation` is not implemented.

- [ ] **Step 3: Commit the failing generation tests**

```bash
git add crates/spur-core/src/skills/projection/generation.rs crates/spur-core/src/skills/projection/mod.rs
git commit -m "test(skills): rsp-3 specify immutable generations"
```

- [ ] **Step 4: Define generation records and typed errors**

Use these core records:

```rust
pub const RENDERER_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredTarget {
    pub skill_id: String,
    pub source: super::resolver::ResolvedSource,
    pub target_rel: String,
    pub generation_rel: String,
    pub target_kind: TargetKind,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedGeneration {
    pub adapter: crate::skills::adapters::Adapter,
    pub digest: String,
    pub root: std::path::PathBuf,
    pub targets: Vec<DesiredTarget>,
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("unsafe source path {path} for skill {skill_id}")]
    UnsafeSourcePath {
        skill_id: String,
        path: std::path::PathBuf,
    },
    #[error("rendered target escaped launch root: {path}")]
    TargetEscaped { path: std::path::PathBuf },
    #[error("generation I/O failed at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("generation hashing failed: {0}")]
    Hash(#[source] anyhow::Error),
}
```

- [ ] **Step 5: Stage supporting assets safely**

Build under `generations/.tmp-<uuid>`. Recursively copy source entries other
than the source `SKILL.md` into `skills/<id>/`, sorting directory entries and
rejecting symlinks or paths outside the source root.

- [ ] **Step 6: Overlay adapter-rendered entry points**

Call the existing adapter renderer and write its rendered bytes into each
per-skill staging directory. Record the real adapter-relative target from the
renderer: directory adapters target the rendered file's parent directory and
Cursor targets its `.mdc` file. Stage `render_kiro_steering_pointer` under
`companions/` and record its file target for Kiro.

- [ ] **Step 7: Hash, publish, and reuse immutable generations**

Hash `renderer-schema-version + adapter-key + staged-tree-hash` and atomically
rename the complete directory to `generations/<digest>`. When the destination
already exists and validates, remove the temp directory and reuse the existing
generation without rewriting.

- [ ] **Step 8: Run generation, adapter, and formatting tests**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::generation::tests -- --nocapture
scripts/spur-cargo test -p spur-core skills::adapters::tests -- --nocapture
scripts/spur-cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 9: Commit the generation builder**

```bash
git add crates/spur-core/src/skills/projection crates/spur-core/src/skills/adapters.rs
git commit -m "feat(skills): rsp-3 publish adapter generations"
```

---

### Task 4: Reconcile projections with ownership, fallback, and recovery

**Depends on:** Task 3

**Files:**
- Create: `crates/spur-core/src/skills/projection/manifest.rs`
- Create: `crates/spur-core/src/skills/projection/reconcile.rs`
- Modify: `crates/spur-core/src/skills/projection/mod.rs`
- Modify: `crates/spur-core/src/skills/installer.rs:20-230`
- Modify: `crates/spur-core/src/explore/materialize.rs:169-260`

- [ ] **Step 1: Add failing reconciliation tests**

Add tests for link creation, copy fallback, user collision, stale cleanup,
legacy marker adoption, modified-copy preservation, idempotence, concurrent
locking, rollback, pending-journal recovery, and generation GC. The copy
fallback test must inject failure rather than depending on host permissions:

```rust
#[tokio::test]
async fn symlink_failure_falls_back_to_tracked_copy() {
    let fixture = ProjectionFixture::new(Adapter::Codex);
    fixture.write_bundled_skill("copy-me", "brain", "BODY");

    let summary = reconcile_with_linker(
        fixture.worktrees(),
        fixture.request(),
        &AlwaysFailSymlink,
    )
    .await
    .unwrap();

    let target = fixture
        .launch_root()
        .join(".codex/skills/spurpower-copy-me");
    assert!(target.join("SKILL.md").is_file());
    assert_eq!(summary.copied, vec![target]);
}
```

The collision test writes an unmarked directory at the desired target and
asserts byte-for-byte preservation plus one skip whose reason is
`ProjectionSkipReason::UserOwned`.
The recovery test writes a valid old manifest and `pending.json`, switches one
target to the new generation, and asserts the next reconcile completes or
rolls back before applying a fresh generation.

Define the injected link seam used by those tests:

```rust
trait Linker {
    fn symlink(
        &self,
        source: &std::path::Path,
        target: &std::path::Path,
        kind: super::generation::TargetKind,
    ) -> std::io::Result<()>;
}

struct AlwaysFailSymlink;

impl Linker for AlwaysFailSymlink {
    fn symlink(
        &self,
        _source: &std::path::Path,
        _target: &std::path::Path,
        _kind: super::generation::TargetKind,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected symlink denial",
        ))
    }
}
```

- [ ] **Step 2: Run the reconciliation tests and confirm they fail**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection::reconcile::tests -- --nocapture
```

Expected: FAIL because the manifest and reconciler do not exist.

- [ ] **Step 3: Commit the failing transaction tests**

```bash
git add crates/spur-core/src/skills/projection
git commit -m "test(skills): rsp-4 specify safe reconciliation"
```

- [ ] **Step 4: Implement the versioned manifest and pending journal**

Use schema version 1 and these serialized shapes:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectionManifest {
    pub schema_version: u32,
    pub renderer_schema_version: u32,
    pub adapter: String,
    pub role: super::RuntimeRole,
    pub policy: super::SelectionPolicy,
    pub generation: String,
    pub targets: Vec<TargetRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMode {
    Symlink,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetRecord {
    pub skill_id: String,
    pub source_kind: super::resolver::ResolvedSourceKind,
    pub source_sha256: String,
    pub target_rel: String,
    pub generation_rel: String,
    pub mode: ProjectionMode,
    pub projected_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingTransaction {
    pub schema_version: u32,
    pub prior: Option<ProjectionManifest>,
    pub next: ProjectionManifest,
    pub operations: Vec<PendingOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedTargetState {
    Absent,
    Symlink { destination: String },
    Copy { content_sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingOperation {
    pub target_rel: String,
    pub prior_state: RecordedTargetState,
    pub next: Option<TargetRecord>,
    pub backup_rel: Option<String>,
}
```

Atomic JSON writes use a sibling `NamedTempFile`, `write_all`, `sync_all`, and
`persist`. A malformed present manifest or journal is fatal and reports its
path; a missing file is not an error.

- [ ] **Step 5: Acquire the adapter lock and recover pending transactions**

Use `fs4::fs_std::FileExt::lock_exclusive` on `reconcile.lock`. Add a private
`Linker` trait with Unix and Windows implementations and a test-only failing
implementation. When `pending.json` exists, validate its prior manifest and
recorded target states, then complete or roll back that transaction before
resolving a new generation.

- [ ] **Step 6: Classify desired and stale targets without mutating disk**

Classify every path against the prior manifest, recovered pending record,
validated legacy marker, or legacy pool materialization record. Preserve and
report unowned or edited targets. Build the complete `PendingOperation` list
and atomically write `pending.json` before switching any path.

- [ ] **Step 7: Install relative links with copy fallback**

For each owned path, rename the old target to the operation's unique sibling
backup. Create a relative symlink through a temp name and atomically rename it
to the target. If the injected `Linker` returns an error, recursively copy the
generation target to a temp sibling, hash the copy, and atomically rename the
copy to the target.

- [ ] **Step 8: Add rollback and stale-target removal**

On any ordinary error, remove newly installed targets and restore backups in
reverse operation order. Remove a stale target only when its current symlink
destination or copy digest equals `prior_state`; otherwise preserve it as
`ProjectionSkipReason::OwnershipLost`.

- [ ] **Step 9: Commit the manifest, excludes, and generation GC**

Call `WorktreeManager::add_worktree_excludes` with every managed target and
`.spur/runtime/skill-projections/`. Atomically commit `manifest.json`, remove
`pending.json` and backups, then collect generations unreferenced by the new
manifest or preserved targets.

- [ ] **Step 10: Add legacy ownership adapters**

Expose a marker-validation helper from `installer.rs`; do not duplicate marker
parsing. Expose a legacy materialization-record query from
`explore::materialize` so migration can prove `(worktree, skill ID)` ownership.

- [ ] **Step 11: Implement the locked public entry points and display summary**

`reconcile` constructs a `WorktreeManager` from `source_repo_root` and delegates
to `reconcile_with_worktrees`. `reconcile_many` processes adapters in their
input order using `RuntimeRole::Init` and `SelectionPolicy::AllActive`.
`ProjectionSummary::fmt` prints linked, copied, unchanged, removed, migrated,
and skipped counts plus per-path warnings.

- [ ] **Step 12: Run focused transaction tests**

Run:

```bash
scripts/spur-cargo test -p spur-core skills::projection -- --nocapture
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
scripts/spur-cargo fmt --all -- --check
```

Expected: PASS, including recovery and copy fallback.

- [ ] **Step 13: Commit the reconciler**

```bash
git add crates/spur-core/src/skills crates/spur-core/src/explore/materialize.rs
git commit -m "feat(skills): rsp-4 reconcile owned projections"
```

---

### Task 5: Route `spur skills init` through runtime projection

**Depends on:** Task 4

**Files:**
- Modify: `crates/spur-cli/src/commands/init.rs:203-217, 718-746`
- Modify: `crates/spur-cli/src/main.rs:1153-1166`
- Create: `crates/spur-cli/tests/skills_init_projection.rs`

- [ ] **Step 1: Add a failing CLI initialization test**

Create a temporary Git repository and a temporary bundled asset root, write the
asset root into `.spur/config.toml`, then execute the compiled CLI:

```rust
#[test]
fn skills_init_creates_runtime_manifest_and_all_bundled_targets() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());
    let assets = write_asset_root(repo.path(), "brain-only", "brain");
    write_skills_config(repo.path(), &assets);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["skills", "init"])
        .current_dir(repo.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(repo.path().join(
        ".spur/runtime/skill-projections/codex/manifest.json"
    ).exists());
    assert!(repo.path().join(
        ".codex/skills/spurpower-brain-only/SKILL.md"
    ).exists());
}
```

Assert the output summary contains `linked` or `copied` and `git status
--porcelain` does not list projection artifacts.

Define these helpers locally in the integration test:

```rust
fn init_git_repo(root: &std::path::Path);
fn write_asset_root(
    repo_root: &std::path::Path,
    id: &str,
    role: &str,
) -> std::path::PathBuf;
fn write_skills_config(repo_root: &std::path::Path, assets: &std::path::Path);
```

`init_git_repo` runs `git init`, sets a local test identity, and commits an
empty baseline. `write_asset_root` creates `<repo>/test-assets/<id>/SKILL.md`.
`write_skills_config` writes an absolute `bundled_dir` under `[skills]`.

- [ ] **Step 2: Run the CLI test and confirm direct installer behavior fails it**

Run:

```bash
scripts/spur-cargo test -p spur-cli --test skills_init_projection -- --nocapture
```

Expected: FAIL because the CLI does not create a runtime manifest.

- [ ] **Step 3: Commit the failing CLI test**

```bash
git add crates/spur-cli/tests/skills_init_projection.rs
git commit -m "test(spur-cli): rsp-5 require projection init"
```

- [ ] **Step 4: Make CLI initialization async and use the shared service**

Change both command helpers to async:

```rust
pub async fn run_skills_init(repo_root: &std::path::Path) -> Result<()> {
    let summaries = spur_core::skills::projection::reconcile_many(
        repo_root,
        repo_root,
        spur_core::skills::adapters::Adapter::all(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("skills projection failed: {error}"))?;
    println!();
    for summary in summaries {
        print!("{summary}");
    }
    print_gitattributes_advisory_if_needed(repo_root);
    Ok(())
}
```

`run_skills_init_filtered` passes its adapter slice to the same service. Await
both helpers in `commands::init::run`; await the standalone `SkillsCommands::Init`
arm in `main.rs`. Keep the current warning behavior for the multi-step `spur
init` flow, while standalone `spur skills init` returns projection failure.

- [ ] **Step 5: Run CLI and installer compatibility tests**

Run:

```bash
scripts/spur-cargo test -p spur-cli --test skills_init_projection -- --nocapture
scripts/spur-cargo test -p spur-cli init_ux -- --nocapture
scripts/spur-cargo test -p spur-core --test skills_installer -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit CLI integration**

```bash
git add crates/spur-cli/src/commands/init.rs crates/spur-cli/src/main.rs crates/spur-cli/tests/skills_init_projection.rs
git commit -m "feat(spur-cli): rsp-5 initialize skill projections"
```

---

### Task 6: Make worker startup reconcile all active skills before session spawn

**Depends on:** Task 4

**Files:**
- Modify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:1001-1038, 2607-2661, 4215-4295`
- Modify: `crates/spur-core/src/explore/materialize.rs:29-167, 327-562`

- [ ] **Step 1: Update the existing pre-session worker test to the new contract**

Rename `attempt_materializes_pool_skills_before_session` to
`attempt_reconciles_all_active_skills_before_session`. Update
`SkillMaterializationConnection::new_session` to check both:

```rust
for relative in [
    ".codex/skills/spurpower-clean-a/SKILL.md",
    ".codex/skills/spurpower-test-driven-development/SKILL.md",
] {
    let path = cwd.join(relative);
    assert!(path.exists(), "skill missing before session: {}", path.display());
    let ignored = Command::new("git")
        .args(["check-ignore", relative])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(ignored.status.success(), "{relative} is not excluded");
}
```

Add a second accepted pool skill while leaving `ctx.skills` as a one-item list;
assert both pool skills appear to prove v1 ignores per-delegation narrowing.
Add an invalid pool digest case and assert `run_one_worker_attempt` returns an
error before the fake connection's `new_session` flag becomes true.

- [ ] **Step 2: Run the worker tests and confirm old direct-write behavior fails**

Run:

```bash
scripts/spur-cargo test -p spur-core profile_override_tests::attempt_reconciles_all_active_skills_before_session -- --exact
scripts/spur-cargo test -p spur-core profile_override_tests::projection_failure_prevents_worker_session -- --exact
```

Expected: FAIL because pool skills are unprefixed, built-ins are absent, and
materialization errors are fail-soft.

- [ ] **Step 3: Commit the failing worker integration tests**

```bash
git add crates/spur-core/src/orchestrator/delegation/worker_attempt.rs
git commit -m "test(spur-core): rsp-6 require worker projection"
```

- [ ] **Step 4: Replace the pool-only hook with fatal shared reconciliation**

Immediately after profile materialization and before connection creation, call:

```rust
let projection_summary = if let Some(adapter) =
    crate::skills::adapters::Adapter::for_agent_kind(ctx.agent_config.kind)
{
    Some(
        crate::skills::projection::reconcile_with_worktrees(
            worktrees,
            crate::skills::projection::ProjectionRequest {
                source_repo_root: &worktrees.repo_root,
                launch_root: &worktree_info.path,
                adapter,
                role: crate::skills::projection::RuntimeRole::Worker,
                policy: crate::skills::projection::SelectionPolicy::AllActive,
            },
        )
        .await
        .with_context(|| format!("project skills for worker {}", ctx.agent))?,
    )
} else {
    tracing::debug!(agent = %ctx.agent, "agent kind has no skill adapter");
    None
};
```

Append the legacy Explore materialization record from the summary's selected
pool sources for Manage-view compatibility. Recording failure remains a
warning because it is observability metadata, not projection correctness.
Delete the old direct-write loop, ownership helper, and requested-subset tests
from `explore::materialize`; retain pool loading and record reader/writer helpers
used by resolution and migration.

- [ ] **Step 5: Run worker, projection, and Explore tests**

Run:

```bash
scripts/spur-cargo test -p spur-core profile_override_tests::attempt_reconciles_all_active_skills_before_session -- --exact
scripts/spur-cargo test -p spur-core profile_override_tests::projection_failure_prevents_worker_session -- --exact
scripts/spur-cargo test -p spur-core explore::materialize::tests -- --nocapture
scripts/spur-cargo test -p spur-core skills::projection -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit worker startup integration**

```bash
git add crates/spur-core/src/orchestrator/delegation/worker_attempt.rs crates/spur-core/src/explore/materialize.rs
git commit -m "feat(spur-core): rsp-6 project worker skills"
```

---

### Task 7: Make every brain connection reconcile skills before initialization

**Depends on:** Task 4

**Files:**
- Modify: `crates/spur-core/src/orchestrator/connection.rs:146-215`

- [ ] **Step 1: Add failing brain preflight tests**

Extract a testable preflight method and test it without spawning an ACP process:

```rust
#[tokio::test]
async fn brain_preflight_projects_skills_before_connection_creation() {
    let fixture = BrainProjectionFixture::codex();
    let (brain_name, config, summary) = fixture
        .orchestrator
        .prepare_brain_runtime(Some("codex"))
        .await
        .unwrap();

    assert_eq!(brain_name, "codex");
    assert_eq!(config.kind, spur_acp::types::AgentKind::CodexAcp);
    let summary = summary.unwrap();
    assert!(summary
        .linked
        .iter()
        .chain(summary.copied.iter())
        .any(|path| path.ends_with(".codex/skills/spurpower-brainstorming")));
}
```

Add a corrupt accepted-pool digest case asserting `prepare_brain_runtime`
returns an error. The method must return before any connection factory or
`initialize` call can occur because it returns the resolved config only after
projection succeeds.

Define the local test fixture as:

```rust
struct BrainProjectionFixture {
    _repo: tempfile::TempDir,
    orchestrator: crate::Orchestrator,
}

impl BrainProjectionFixture {
    fn codex() -> Self;
    fn corrupt_pool_digest(&self);
}
```

`codex` initializes Git, writes a temporary bundled-root config, registers an
`AgentConfig` named `codex` with `AgentKind::CodexAcp`, and returns an
orchestrator rooted at the temp repo. `corrupt_pool_digest` adds an accepted
pool item whose recorded digest differs from its vendored directory hash.

- [ ] **Step 2: Run the preflight tests and confirm the method is missing**

Run:

```bash
scripts/spur-cargo test -p spur-core orchestrator::connection::tests::brain_preflight_projects_skills_before_connection_creation -- --exact
scripts/spur-cargo test -p spur-core orchestrator::connection::tests::brain_projection_failure_prevents_connection_creation -- --exact
```

Expected: FAIL because `prepare_brain_runtime` does not exist.

- [ ] **Step 3: Commit the failing brain tests**

```bash
git add crates/spur-core/src/orchestrator/connection.rs
git commit -m "test(spur-core): rsp-7 require brain projection"
```

- [ ] **Step 4: Add preflight and call it from `connect_brain`**

Implement this method on `Orchestrator`:

```rust
async fn prepare_brain_runtime(
    &self,
    brain_override: Option<&str>,
) -> anyhow::Result<(
    String,
    spur_acp::config::AgentConfig,
    Option<crate::skills::projection::ProjectionSummary>,
)> {
    let brain_name = self.selected_brain_name(brain_override);
    let config = self
        .registry
        .get(&brain_name)
        .ok_or_else(|| anyhow::anyhow!("Brain agent '{}' not found in registry", brain_name))?
        .clone();
    let summary = match crate::skills::adapters::Adapter::for_agent_kind(config.kind) {
        Some(adapter) => Some(crate::skills::projection::reconcile(
            crate::skills::projection::ProjectionRequest {
                source_repo_root: &self.repo_root,
                launch_root: &self.repo_root,
                adapter,
                role: crate::skills::projection::RuntimeRole::Brain,
                policy: crate::skills::projection::SelectionPolicy::AllActive,
            },
        ).await?),
        None => None,
    };
    Ok((brain_name, config, summary))
}
```

At the start of `connect_brain`, replace its duplicate name/config lookup with
`prepare_brain_runtime(brain_override).await?`, then create and initialize the
connection.
Because all three current callers use `connect_brain`, initial sessions,
interactive direct connections, and reconnects receive the same preflight.
Log the projection summary at `debug`; projection errors remain fatal.

- [ ] **Step 5: Run brain and connection tests**

Run:

```bash
scripts/spur-cargo test -p spur-core orchestrator::connection::tests -- --nocapture
scripts/spur-cargo test -p spur-core session_milestone_events -- --nocapture
```

Expected: PASS. Existing connection errors retain their context.

- [ ] **Step 6: Commit brain startup integration**

```bash
git add crates/spur-core/src/orchestrator/connection.rs
git commit -m "feat(spur-core): rsp-7 project brain skills"
```

---

### Task 8: Add cross-path acceptance coverage and verify the workspace

**Depends on:** Tasks 5, 6, and 7

**Files:**
- Create: `crates/spur-core/tests/runtime_skill_projection.rs`

- [ ] **Step 1: Add an end-to-end projection acceptance test**

The test creates a temporary Git repo with:

- a `role: brain` bundled skill;
- a bundled skill replaced by an accepted `replaced-bundled` pool item;
- a second accepted pool-only skill;
- a repository override with highest precedence;
- an unmarked user-owned Codex skill target.

Call the public projection API twice and assert:

```rust
let first = reconcile(request(&repo, Adapter::Codex, RuntimeRole::Worker))
    .await
    .unwrap();
assert!(!first.linked.is_empty() || !first.copied.is_empty());
assert_eq!(read_body(&repo, "pool-replaces"), "POOL BODY\n");
assert_eq!(read_body(&repo, "repo-wins"), "REPOSITORY BODY\n");
assert_eq!(read_body(&repo, "brain-builtin"), "BUNDLED BRAIN BODY\n");
assert_eq!(read_user_collision(&repo), "USER OWNED\n");

let second = reconcile(request(&repo, Adapter::Codex, RuntimeRole::Worker))
    .await
    .unwrap();
assert!(second.linked.is_empty());
assert!(second.copied.is_empty());
assert!(!second.unchanged.is_empty());
assert!(git_status(&repo).is_empty());
```

Remove the pool-only manifest item, reconcile again, and assert its unchanged
SPUR-owned target is removed. Modify one copy fallback before removal and assert
the edited target is preserved and reported as skipped.

Define the acceptance helpers in the integration test with these signatures:

```rust
fn request(
    repo: &std::path::Path,
    adapter: spur_core::skills::adapters::Adapter,
    role: spur_core::skills::projection::RuntimeRole,
) -> spur_core::skills::projection::ProjectionRequest<'_>;
fn read_body(repo: &std::path::Path, id: &str) -> String;
fn read_user_collision(repo: &std::path::Path) -> String;
fn git_status(repo: &std::path::Path) -> String;
```

Fixture setup uses public `explore::pool` and `explore::store` APIs, writes a
project `[skills].bundled_dir`, and commits only the user-owned baseline before
projection.

- [ ] **Step 2: Run the acceptance test**

Run:

```bash
scripts/spur-cargo test -p spur-core --test runtime_skill_projection -- --nocapture
```

Expected: PASS. If it fails, fix only the acceptance fixture or implementation
defect directly exposed by this test; do not broaden scope.

- [ ] **Step 3: Commit acceptance coverage**

```bash
git add crates/spur-core/tests/runtime_skill_projection.rs
git commit -m "test(spur-core): rsp-8 cover runtime projection"
```

- [ ] **Step 4: Run formatting and focused crate suites**

Run:

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-core
scripts/spur-cargo test -p spur-cli
```

Expected: all commands exit 0.

- [ ] **Step 5: Run remote Clippy with warnings denied**

Run:

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core -p spur-cli --all-targets -- -D warnings
```

Expected: exit 0 with no warnings.

- [ ] **Step 6: Record verification in the beads completion audit**

The worker completion must list the exact commands above, their exit codes,
the final commit SHA, and any platform limitation not exercised directly. Do
not close the issue; the brain reviews and approves it.

---

## Plan DAG

```text
rsp-1-bundled-policy
  -> rsp-2-effective-resolution
    -> rsp-3-generation-builder
      -> rsp-4-safe-reconciler
        +-> rsp-5-cli-init --------+
        +-> rsp-6-worker-launch ---+-> rsp-8-acceptance
        +-> rsp-7-brain-launch ----+
```

Tasks 5, 6, and 7 own disjoint integration files and may run concurrently only
after Task 4 is approved. Task 8 must wait for all three integrations.

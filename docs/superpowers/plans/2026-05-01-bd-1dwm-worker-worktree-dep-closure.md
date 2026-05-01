# bd-1dwm — Worker Worktree Dep-Closure Integration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate silent in-plan work loss (the bd-2dww failure mode) by making every worker dispatch see the merged contents of its declared transitive dependencies.

**Architecture:** Two-phase delivery. Phase 0 ships a clobber detector (D) as standalone insurance. Phase 1 ships G-strict (stateless dep-closure cherry-pick at dispatch) plus seven companion components: additive `BaseSpec` API, `dispatched_base_oid` persistence, `OverlayConflict` signal routing, new `BlockedOnSetupConflict` plan task status, `preview_task_base` MCP tool, `get_task_diff` fix, single-commit-output invariant, lineage event extension, and WorktreeAuthority v2 migration.

**Tech Stack:** Rust workspace (`spur-core`, `spur-mcp`, `spur-worktree`, `spur-acp`, `spur-tui`), `serde`, `schemars`, `tokio`, `git` (CLI invoked via `tokio::process::Command`), beads MCP backend.

**Spec:** `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md`

---

## File Structure

### Phase 0 — Clobber Detector
- `crates/spur-mcp/src/plan/signals.rs` — extend `WorkerSignal` enum with `PotentialClobber` variant
- `crates/spur-mcp/src/plan/clobber_detector.rs` (NEW) — detector logic + sentinel emission helper
- `crates/spur-mcp/src/server.rs` — wire detector into `review_task` handler
- `crates/spur-tui/src/components/plan_pulse.rs` — surface `signal:potential-clobber` label in task review pane
- `crates/spur-mcp/tests/clobber_detector_integration.rs` (NEW) — end-to-end test

### Phase 1 — G-strict Core + Companions
- `crates/spur-mcp/src/tools.rs` — add `BaseSpec` enum + `OverlayCommit` struct; extend `DelegationRequest` with `Optional<BaseSpec>`
- `crates/spur-mcp/src/tool_schemas.rs` — add Optional `base` to `DelegateToWorkerInput` / `DelegateParallelTaskInput`
- `crates/spur-mcp/src/server.rs` — translate tool input → request `base`; add `preview_task_base` handler; fix `get_task_diff`
- `crates/spur-mcp/src/plan/mod.rs` — add `dispatched_base_oid` to `PlanTaskEntry` + `AttemptRecord`; add `BlockedOnSetupConflict` to `PlanTaskStatus`
- `crates/spur-mcp/src/plan/audit_sentinel.rs` — persist `dispatched_base_oid` in completion sentinels
- `crates/spur-mcp/src/plan/projector.rs` — emit/consume `dispatched_base_oid`
- `crates/spur-mcp/src/plan/reconciler.rs` — compute overlay closure for plan tasks; route `OverlayConflict` to `signal:integration-conflict`
- `crates/spur-worktree/src/manager.rs` — add `apply_overlays` method; add `WorktreeError::OverlayConflict` error
- `crates/spur-core/src/orchestrator.rs` — apply `BaseSpec` before agent init; record `dispatched_base_oid`; emit lineage event
- `crates/spur-core/src/lineage/adapter.rs` + `SpurEventBody` definition — new `DispatchOverlayApplied` event
- `crates/spur-core/src/worktree_authority.rs` — recognize new v2-named branches
- `crates/spur-tui/src/components/plan_pulse.rs` — render `BlockedOnSetupConflict` distinct from `Failed`
- `crates/spur-mcp/tests/g_strict_e2e.rs` (NEW) — synthetic bd-2dww reproducer

---

## Conventions

- **Test command (single test):** `cargo test --manifest-path crates/<crate>/Cargo.toml <test_name> -- --nocapture`
- **Test command (full crate):** `cargo test --manifest-path crates/<crate>/Cargo.toml`
- **Build check:** `cargo check --workspace`
- **Lint:** `cargo clippy --workspace -- -D warnings`
- **Commit conventions:** Use Conventional Commits prefixes (`feat`, `fix`, `refactor`, `test`, `chore`). Reference issue `bd-1dwm` in commit body.
- **TDD flow per step:** Write failing test → run to verify failure → implement minimal code → run to verify pass → commit.

---

# Phase 0 — Clobber Detector (D)

> **Phase 0 ships as a standalone PR. Cut a release here before starting Phase 1.** Phase 0 provides immediate insurance against bd-2dww-class loss; Phase 1 fixes the structural cause.

---

## Task 1: Add `PotentialClobber` variant to `WorkerSignal`

**Files:**
- Modify: `crates/spur-mcp/src/plan/signals.rs:13-44`

- [ ] **Step 1.1: Write the failing test**

Add to `crates/spur-mcp/src/plan/signals.rs` (in the `tests` module, around line 70):

```rust
#[test]
fn potential_clobber_round_trips_and_has_label() {
    let sig = WorkerSignal::PotentialClobber {
        signal_id: Uuid::nil(),
        conflicting_task_id: "task-1".to_string(),
        file: "crates/spur-tui/src/foo.rs".to_string(),
        upstream_tip: "abc123".to_string(),
        worker_tip: "def456".to_string(),
    };
    let body = encode_comment(&sig);
    let parsed = parse_comment(&body).unwrap().unwrap();
    assert_eq!(parsed, sig);
    assert_eq!(sig.kind_label(), "potential-clobber");
    assert_eq!(sig.signal_id(), Uuid::nil());
}
```

- [ ] **Step 1.2: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml potential_clobber_round_trips_and_has_label`
Expected: FAIL — `PotentialClobber` variant does not exist.

- [ ] **Step 1.3: Add the enum variant**

Edit `crates/spur-mcp/src/plan/signals.rs` — extend `WorkerSignal` enum:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerSignal {
    ScopeDrift {
        signal_id: Uuid,
        severity: f32,
        reason: String,
        #[serde(default)]
        estimated_subtasks: Option<u8>,
    },
    /// Brain-side detector signal: the worker created or modified a file
    /// that overlaps non-trivially with an already-approved upstream task's
    /// tip. Emitted by `clobber_detector` during `review_task`. May also
    /// be emitted by future worker-side guards.
    PotentialClobber {
        signal_id: Uuid,
        conflicting_task_id: String,
        file: String,
        /// OID at the upstream task's tip where the file content lives.
        upstream_tip: String,
        /// OID at the current worker's tip where the conflicting content lives.
        worker_tip: String,
    },
}
```

Update `signal_id()` and `kind_label()`:

```rust
impl WorkerSignal {
    pub fn signal_id(&self) -> Uuid {
        match self {
            WorkerSignal::ScopeDrift { signal_id, .. } => *signal_id,
            WorkerSignal::PotentialClobber { signal_id, .. } => *signal_id,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            WorkerSignal::ScopeDrift { .. } => "scope-drift",
            WorkerSignal::PotentialClobber { .. } => "potential-clobber",
        }
    }
}
```

- [ ] **Step 1.4: Run test to verify it passes**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml potential_clobber_round_trips_and_has_label`
Expected: PASS.

- [ ] **Step 1.5: Confirm no downstream `match` is now non-exhaustive**

Run: `cargo check --workspace`
Expected: clean. The enum is `#[non_exhaustive]` so external matches must already use `_ =>` arms, but in-crate matches that don't will fail. Add `WorkerSignal::PotentialClobber { .. } => /* no-op */` arms to any in-crate match found.

- [ ] **Step 1.6: Commit**

```bash
git add crates/spur-mcp/src/plan/signals.rs
git commit -m "feat(spur-mcp): add PotentialClobber variant to WorkerSignal

Phase 0 of bd-1dwm. Brain-side detector signal that fires when a worker
creates or modifies a file that overlaps non-trivially with an
already-approved upstream task's tip. Carries upstream + worker OIDs
for forensic context.

Refs: bd-1dwm"
```

---

## Task 2: Implement `clobber_detector` helper module

**Files:**
- Create: `crates/spur-mcp/src/plan/clobber_detector.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs` (add `pub mod clobber_detector;`)
- Test: `crates/spur-mcp/src/plan/clobber_detector.rs` (inline `#[cfg(test)]`)

The detector takes a worker branch + a list of prior approved task tips, returns the set of `PotentialClobber` signals that should be emitted.

- [ ] **Step 2.1: Write the failing test**

Create `crates/spur-mcp/src/plan/clobber_detector.rs` with the test module:

```rust
//! Brain-side clobber detector for plan task review.
//!
//! Fires `PotentialClobber` signals when a worker creates or modifies a
//! file that overlaps non-trivially with an already-approved upstream
//! task's tip. See `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md`
//! Phase 0 (D — clobber detector).

use std::path::Path;
use uuid::Uuid;

use crate::plan::signals::WorkerSignal;

/// One upstream approved task's tip used as a clobber-baseline.
#[derive(Debug, Clone)]
pub struct PriorTip {
    pub task_id: String,
    pub branch_name: String,
    pub tip_oid: String,
}

/// Result of running the detector against a worker branch.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorReport {
    pub signals: Vec<WorkerSignal>,
}

/// Minimum byte-size of overlap required to flag a file as a potential
/// clobber. Files smaller than this on the upstream tip are ignored
/// (avoids false positives for trivial files like `mod.rs` re-exports).
pub const MIN_NONTRIVIAL_BYTES: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_repo() -> PathBuf {
        // Real implementation will build a tmp git repo. For the failing
        // test, the helper returns a path that does not exist so the
        // function signature is exercised before the impl lands.
        PathBuf::from("/nonexistent")
    }

    #[test]
    fn detector_flags_overlapping_nontrivial_file() {
        // 1. Build temp git repo with main HEAD containing nothing.
        // 2. Branch `upstream` → commit `foo.rs` with 200 bytes of content A.
        // 3. Branch `worker` from main HEAD → commit `foo.rs` with 200 bytes of different content B.
        // 4. Run detector(repo, "worker", [PriorTip { upstream }]).
        // 5. Assert: one PotentialClobber signal for "foo.rs".
        let repo = fixture_repo();
        let priors = vec![PriorTip {
            task_id: "upstream".into(),
            branch_name: "upstream".into(),
            tip_oid: "deadbeef".into(),
        }];
        let report = run(&repo, "worker", &priors);
        assert_eq!(report.signals.len(), 1);
        match &report.signals[0] {
            WorkerSignal::PotentialClobber { conflicting_task_id, file, .. } => {
                assert_eq!(conflicting_task_id, "upstream");
                assert_eq!(file, "foo.rs");
            }
            _ => panic!("expected PotentialClobber"),
        }
    }

    #[test]
    fn detector_ignores_trivial_files() {
        // File <MIN_NONTRIVIAL_BYTES on upstream → no signal even on overlap.
        let repo = fixture_repo();
        let priors = vec![];
        let report = run(&repo, "worker", &priors);
        assert!(report.signals.is_empty());
    }

    #[test]
    fn detector_ignores_disjoint_files() {
        // Worker creates `bar.rs`, upstream has `foo.rs` → no signal.
        let repo = fixture_repo();
        let priors = vec![];
        let report = run(&repo, "worker", &priors);
        assert!(report.signals.is_empty());
    }
}
```

- [ ] **Step 2.2: Add `pub mod clobber_detector;` to `crates/spur-mcp/src/plan/mod.rs`**

Find the existing `pub mod` declarations near the top of `crates/spur-mcp/src/plan/mod.rs` and add:

```rust
pub mod clobber_detector;
```

- [ ] **Step 2.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml plan::clobber_detector`
Expected: FAIL — `run` function does not exist.

- [ ] **Step 2.4: Implement `run`**

Add to `crates/spur-mcp/src/plan/clobber_detector.rs`:

```rust
/// Run the clobber detector against a worker branch.
///
/// Compares each file in the worker branch's diff vs the plan's base
/// against the same file on each prior approved tip. Emits a signal
/// when:
/// - the file exists on a prior approved tip with content size ≥
///   MIN_NONTRIVIAL_BYTES
/// - AND the worker's content for that file differs from the prior tip's
///   content (judged by hash inequality, not subset/superset).
pub fn run(repo: &Path, worker_branch: &str, priors: &[PriorTip]) -> DetectorReport {
    let mut signals = Vec::new();
    let worker_files = git_diff_name_only(repo, "HEAD", worker_branch).unwrap_or_default();

    for prior in priors {
        let upstream_files = git_ls_tree_files(repo, &prior.branch_name).unwrap_or_default();
        for file in &worker_files {
            if !upstream_files.contains_key(file) {
                continue; // file not present on prior — not a clobber
            }
            let upstream_size = upstream_files[file].size;
            if upstream_size < MIN_NONTRIVIAL_BYTES {
                continue; // trivial file, ignore
            }
            let upstream_blob = git_blob_oid(repo, &prior.branch_name, file).ok();
            let worker_blob = git_blob_oid(repo, worker_branch, file).ok();
            if upstream_blob.is_none() || worker_blob.is_none() {
                continue;
            }
            if upstream_blob == worker_blob {
                continue; // identical content — not a clobber
            }
            signals.push(WorkerSignal::PotentialClobber {
                signal_id: Uuid::new_v4(),
                conflicting_task_id: prior.task_id.clone(),
                file: file.clone(),
                upstream_tip: prior.tip_oid.clone(),
                worker_tip: worker_branch.to_string(),
            });
        }
    }
    DetectorReport { signals }
}

// --- git helpers (thin wrappers over `tokio::process::Command::new("git")`) ---
// These run synchronously via `std::process::Command` because the detector
// is invoked from the synchronous review-handler path. If the detector
// moves async, switch to `tokio::process::Command`.

#[derive(Debug)]
struct GitFileEntry {
    size: usize,
}

fn git_diff_name_only(repo: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .args(["diff", "--name-only", base, head])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git diff exit {}: {}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_ls_tree_files(repo: &Path, rev: &str) -> Result<std::collections::HashMap<String, GitFileEntry>, String> {
    let out = std::process::Command::new("git")
        .args(["ls-tree", "-l", "-r", rev])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git ls-tree failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-tree exit {}: {}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    let mut map = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // Format: "<mode> <type> <sha> <size>\t<path>"
        let mut parts = line.split_whitespace();
        let _mode = parts.next();
        let _typ = parts.next();
        let _sha = parts.next();
        let size_str = parts.next().unwrap_or("0");
        let size = size_str.parse::<usize>().unwrap_or(0);
        let path = line.split('\t').nth(1).unwrap_or("").to_string();
        if !path.is_empty() {
            map.insert(path, GitFileEntry { size });
        }
    }
    Ok(map)
}

fn git_blob_oid(repo: &Path, rev: &str, path: &str) -> Result<String, String> {
    let spec = format!("{rev}:{path}");
    let out = std::process::Command::new("git")
        .args(["rev-parse", &spec])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git rev-parse failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-parse exit {}: {}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
```

- [ ] **Step 2.5: Replace stub test with real git-based fixture**

Update the test module to build a real temp git repo. Replace the `tests` module with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(repo).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README"), "init\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn commit_file(repo: &std::path::Path, branch: &str, path: &str, content: &str) -> String {
        run_git(repo, &["checkout", "-q", "-B", branch, "main"]);
        std::fs::write(repo.join(path), content).unwrap();
        run_git(repo, &["add", path]);
        run_git(repo, &["commit", "-q", "-m", &format!("add {path}")]);
        let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn detector_flags_overlapping_nontrivial_file() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &"a".repeat(200));
        let _worker_tip = commit_file(dir.path(), "worker", "foo.rs", &"b".repeat(200));
        let priors = vec![PriorTip { task_id: "upstream".into(), branch_name: "upstream".into(), tip_oid: upstream_tip }];
        let report = run(dir.path(), "worker", &priors);
        assert_eq!(report.signals.len(), 1);
        match &report.signals[0] {
            WorkerSignal::PotentialClobber { conflicting_task_id, file, .. } => {
                assert_eq!(conflicting_task_id, "upstream");
                assert_eq!(file, "foo.rs");
            }
            _ => panic!("expected PotentialClobber"),
        }
    }

    #[test]
    fn detector_ignores_trivial_files() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "tiny.rs", "x"); // 1 byte
        let _worker_tip = commit_file(dir.path(), "worker", "tiny.rs", "y");
        let priors = vec![PriorTip { task_id: "upstream".into(), branch_name: "upstream".into(), tip_oid: upstream_tip }];
        let report = run(dir.path(), "worker", &priors);
        assert!(report.signals.is_empty(), "trivial files should not flag");
    }

    #[test]
    fn detector_ignores_disjoint_files() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &"a".repeat(200));
        let _worker_tip = commit_file(dir.path(), "worker", "bar.rs", &"b".repeat(200));
        let priors = vec![PriorTip { task_id: "upstream".into(), branch_name: "upstream".into(), tip_oid: upstream_tip }];
        let report = run(dir.path(), "worker", &priors);
        assert!(report.signals.is_empty(), "disjoint files should not flag");
    }

    #[test]
    fn detector_ignores_identical_content() {
        let dir = init_repo();
        let same = "a".repeat(200);
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &same);
        let _worker_tip = commit_file(dir.path(), "worker", "foo.rs", &same);
        let priors = vec![PriorTip { task_id: "upstream".into(), branch_name: "upstream".into(), tip_oid: upstream_tip }];
        let report = run(dir.path(), "worker", &priors);
        assert!(report.signals.is_empty(), "identical content should not flag");
    }
}
```

Add `tempfile` to `crates/spur-mcp/Cargo.toml` `[dev-dependencies]` if not already present:

```bash
grep -q 'tempfile' crates/spur-mcp/Cargo.toml || cargo add --manifest-path crates/spur-mcp/Cargo.toml --dev tempfile
```

- [ ] **Step 2.6: Run all four tests**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml plan::clobber_detector`
Expected: PASS (4/4).

- [ ] **Step 2.7: Commit**

```bash
git add crates/spur-mcp/src/plan/clobber_detector.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/Cargo.toml
git commit -m "feat(spur-mcp): clobber_detector helper for plan review

Phase 0 of bd-1dwm. Compares worker branch's modified files against
prior approved task tips. Emits PotentialClobber signal when:
- file exists on prior tip with size >= MIN_NONTRIVIAL_BYTES (64)
- AND worker's content differs from prior's

Trivial files, disjoint files, and identical content are ignored.

Refs: bd-1dwm"
```

---

## Task 3: Wire `clobber_detector` into `review_task` handler

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` — find the `review_task` MCP tool handler (search for `fn review_task` or `"review_task"`), invoke detector before returning approval
- Test: `crates/spur-mcp/tests/clobber_detector_integration.rs` (NEW)

The detector runs at the moment the brain calls `review_task(approve)`. If signals fire, the handler:
1. Writes each `WorkerSignal` as a sentinel comment on the task's beads issue (using `signals::encode_comment`).
2. Adds `signal:potential-clobber` label to the issue.
3. Surfaces the signals in the approval response so the brain sees them inline.

The signals are advisory — they don't block approval. Brain decides whether to reject after seeing them.

- [ ] **Step 3.1: Locate the review handler**

Run: `grep -n '"review_task"' crates/spur-mcp/src/server.rs`
Note the line numbers.

Run: `grep -n 'fn handle_review_task\|async fn review_task' crates/spur-mcp/src/server.rs`
Note the function name + line.

- [ ] **Step 3.2: Write the failing integration test**

Create `crates/spur-mcp/tests/clobber_detector_integration.rs`:

```rust
//! E2E test: a plan with two tasks where T2 clobbers a file from T1.
//! Approving T2 should emit a `signal:potential-clobber` label on the
//! T2 issue and a sentinel-fenced comment with the conflicting file.

// This test reuses the existing test harness pattern from
// `crates/spur-mcp/tests/e2e_closure_v0e.rs`. Refer to that file for
// helper functions like `start_test_server`, `submit_plan`, etc.

#[tokio::test]
async fn approving_clobbering_worker_emits_potential_clobber_signal() {
    // 1. Spin up MCP server with mock backend.
    // 2. Submit plan with two tasks: T1 creates foo.rs (200 bytes "A");
    //    T2 depends on nothing, also creates foo.rs (200 bytes "B").
    // 3. Approve T1 → its worker_branch is recorded.
    // 4. Approve T2 → handler runs clobber detector against [T1's tip].
    // 5. Assert: T2's beads issue has label "signal:potential-clobber".
    // 6. Assert: T2's beads issue has a comment starting with
    //    "[[spur-signal v1]]" containing JSON with kind="potential_clobber"
    //    and file="foo.rs".
    todo!("flesh out using e2e_closure_v0e.rs harness pattern");
}
```

(The `todo!()` is intentional — the test will be filled in fully in Step 3.4 after the impl lands. For now we want a compilable test file.)

- [ ] **Step 3.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml --test clobber_detector_integration`
Expected: FAIL with `not yet implemented` (the `todo!`).

- [ ] **Step 3.4: Implement detector wiring in `review_task`**

In `crates/spur-mcp/src/server.rs`, locate the `review_task` handler. After the brain's approval is processed (status set to `AwaitingReview` → `Approved`) but before the response is returned, add detector invocation. Skeleton:

```rust
// Inside review_task handler, after approval semantics are processed
// and the worker branch tip is known.

if action == "approve" {
    use crate::plan::clobber_detector::{self, PriorTip};

    // Collect prior approved tips from the same plan (excluding the task being approved).
    let priors: Vec<PriorTip> = plan_state
        .tasks
        .iter()
        .filter(|e| matches!(e.status, PlanTaskStatus::Approved { .. }))
        .filter(|e| e.spec.task_id != task_id)
        .filter_map(|e| {
            let branch = e.worker_branch.as_ref()?.clone();
            let tip_oid = e.dispatched_base_oid.clone() // post-Phase-1 use; pre-Phase-1 fall back to git rev-parse on the branch
                .or_else(|| git_rev_parse(repo_root, &branch).ok())?;
            Some(PriorTip {
                task_id: e.spec.task_id.clone(),
                branch_name: branch,
                tip_oid,
            })
        })
        .collect();

    let worker_branch = current_entry.worker_branch.as_deref().unwrap_or_default();
    if !worker_branch.is_empty() && !priors.is_empty() {
        let report = clobber_detector::run(repo_root, worker_branch, &priors);
        for signal in &report.signals {
            // Write sentinel comment.
            let comment_body = crate::plan::signals::encode_comment(signal);
            backend.update_issue_comment(&issue_id, &comment_body).await?;
        }
        if !report.signals.is_empty() {
            backend.update_issue_labels(&issue_id, &["signal:potential-clobber"], &[]).await?;
            // Include in response so the brain sees the signals inline.
            response.signals = report.signals.clone();
        }
    }
}
```

(The exact field names and helper functions like `git_rev_parse`, `update_issue_comment`, and `update_issue_labels` may differ — adapt to the actual codebase names. Search for existing label/comment-write call sites in `server.rs` to match the pattern.)

- [ ] **Step 3.5: Flesh out the integration test**

Replace the `todo!()` body in `crates/spur-mcp/tests/clobber_detector_integration.rs`:

```rust
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approving_clobbering_worker_emits_potential_clobber_signal() {
    // Use the test harness from e2e_closure_v0e.rs (or refactor that harness
    // into a shared `tests/common/mod.rs` if it isn't already).
    let harness = TestHarness::new().await;

    let plan = harness.submit_plan(serde_json::json!({
        "tasks": [
            {
                "task_id": "T1",
                "agent": "mock",
                "task": "create foo.rs with content A",
                "depends_on": [],
            },
            {
                "task_id": "T2",
                "agent": "mock",
                "task": "create foo.rs with content B",
                "depends_on": [],
            },
        ]
    })).await;

    // Drive T1: dispatch → worker creates foo.rs ("A" * 200) → approve.
    harness.dispatch_and_approve("T1", &[("foo.rs", &"A".repeat(200))]).await;

    // Drive T2: dispatch → worker creates foo.rs ("B" * 200) → approve.
    let approve_result = harness.dispatch_and_approve("T2", &[("foo.rs", &"B".repeat(200))]).await;

    // Assert: response carries at least one PotentialClobber signal.
    let signals = approve_result.signals.expect("response must include signals field");
    assert_eq!(signals.len(), 1);
    match &signals[0] {
        crate::plan::signals::WorkerSignal::PotentialClobber { conflicting_task_id, file, .. } => {
            assert_eq!(conflicting_task_id, "T1");
            assert_eq!(file, "foo.rs");
        }
        _ => panic!("expected PotentialClobber"),
    }

    // Assert: T2's issue has the label.
    let labels = harness.get_issue_labels("T2").await;
    assert!(labels.iter().any(|l| l == "signal:potential-clobber"));

    // Assert: T2's issue has a sentinel comment.
    let comments = harness.get_issue_comments("T2").await;
    assert!(comments.iter().any(|c| c.starts_with("[[spur-signal v1]]") && c.contains("potential_clobber")));
}
```

If `TestHarness` doesn't exist as a shared util, refactor the existing helpers in `crates/spur-mcp/tests/e2e_closure_v0e.rs` into `crates/spur-mcp/tests/common/mod.rs` first, then re-import.

- [ ] **Step 3.6: Run test to verify it passes**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml --test clobber_detector_integration`
Expected: PASS.

- [ ] **Step 3.7: Run full crate test suite to catch regressions**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml`
Expected: all green.

- [ ] **Step 3.8: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/clobber_detector_integration.rs crates/spur-mcp/tests/common/mod.rs
git commit -m "feat(spur-mcp): wire clobber_detector into review_task

Phase 0 of bd-1dwm. After approval semantics are processed, run the
detector against prior approved task tips in the same plan. For each
signal:
- write sentinel comment via signals::encode_comment
- add signal:potential-clobber label to the issue
- surface in approval response so brain sees inline

Signals are advisory; brain decides whether to reject after seeing them.

Refs: bd-1dwm"
```

---

## Task 4: Surface `signal:potential-clobber` in TUI plan-pulse pane

**Files:**
- Modify: `crates/spur-tui/src/components/plan_pulse.rs`

The TUI's plan-pulse pane already renders task statuses + labels. Add a visual marker (e.g., colored badge) when a task carries `signal:potential-clobber`.

- [ ] **Step 4.1: Locate the label-rendering site**

Run: `grep -n 'signal:\|labels\b' crates/spur-tui/src/components/plan_pulse.rs | head -20`
Note where labels are read and rendered.

- [ ] **Step 4.2: Write the failing snapshot/unit test**

Find the existing snapshot test for plan_pulse (likely `crates/spur-tui/tests/plan_pulse_snapshot.rs` or inline). Add a new case:

```rust
#[test]
fn plan_pulse_renders_potential_clobber_badge() {
    let task = TaskRow {
        task_id: "T2".into(),
        status: TaskStatus::Approved,
        labels: vec!["signal:potential-clobber".into()],
        // ... other fields
    };
    let rendered = render_task_row(&task);
    assert!(rendered.contains("⚠ CLOBBER"), "rendered output: {rendered}");
}
```

(Adapt struct field names to whatever `plan_pulse.rs` actually uses.)

- [ ] **Step 4.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-tui/Cargo.toml plan_pulse_renders_potential_clobber_badge`
Expected: FAIL — no badge rendering exists.

- [ ] **Step 4.4: Implement the badge**

In `crates/spur-tui/src/components/plan_pulse.rs`, find the label-rendering code path. Add:

```rust
if task.labels.iter().any(|l| l == "signal:potential-clobber") {
    // Render a colored badge before the task title.
    spans.push(Span::styled(
        " ⚠ CLOBBER ",
        Style::default().fg(Color::Black).bg(Color::Yellow),
    ));
}
```

Match the existing `Span` / `Style` import paths — usually `ratatui::text::Span` and `ratatui::style::{Color, Style}`.

- [ ] **Step 4.5: Run test to verify it passes**

Run: `cargo test --manifest-path crates/spur-tui/Cargo.toml plan_pulse_renders_potential_clobber_badge`
Expected: PASS.

- [ ] **Step 4.6: Commit**

```bash
git add crates/spur-tui/src/components/plan_pulse.rs crates/spur-tui/tests/
git commit -m "feat(spur-tui): render potential-clobber badge in plan-pulse

Phase 0 of bd-1dwm. When a task issue carries signal:potential-clobber,
render a yellow ⚠ CLOBBER badge in the plan-pulse pane so the operator
sees it at a glance.

Refs: bd-1dwm"
```

---

## ✅ Phase 0 Boundary

> **STOP HERE if shipping Phase 0 as a standalone PR.**
>
> At this point:
> - `WorkerSignal::PotentialClobber` exists and round-trips
> - `clobber_detector` module is unit-tested with 4 cases
> - `review_task` invokes the detector and writes sentinel + label
> - TUI surfaces the label as a visible badge
> - Integration test exercises the full path
>
> Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`. If both pass, this is ready for PR review and merge.
>
> Phase 1 below depends on Phase 0 only via the shared `WorkerSignal` enum extension. No code-level dependency; you can merge Phase 0 and rebase Phase 1 onto it.

---

# Phase 1 — G-strict Core + Companions

> **Phase 1 is the structural fix.** It introduces `BaseSpec` (additive Optional API), `dispatched_base_oid` persistence, `WorktreeManager::apply_overlays`, orchestrator wiring, reconciler overlay computation, conflict routing, `preview_task_base` MCP tool, `get_task_diff` correction, single-commit invariant, lineage event extension, and WorktreeAuthority migration.
>
> Tasks 5–17 must be executed in order — each builds on the schemas and helpers introduced earlier.

---

## Task 5: Add `BaseSpec` + `OverlayCommit` types (Optional, BWC-safe)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` — add `BaseSpec` enum, `OverlayCommit` struct; extend `DelegationRequest`
- Modify: `crates/spur-mcp/src/tool_schemas.rs` — add Optional `base` to `DelegateToWorkerInput` and `DelegateParallelTaskInput`
- Test: inline `#[cfg(test)]` in `tools.rs`

- [ ] **Step 5.1: Write the failing test**

Add to `crates/spur-mcp/src/tools.rs` (or create `#[cfg(test)] mod tests` if absent):

```rust
#[cfg(test)]
mod base_spec_tests {
    use super::*;

    #[test]
    fn legacy_delegate_input_without_base_deserializes_as_none() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            serde_json::from_value(json).expect("legacy input must parse");
        assert!(parsed.base.is_none(), "missing base must default to None");
    }

    #[test]
    fn delegate_input_with_base_repo_main_deserializes() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
            "base": { "kind": "repo_main" },
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput = serde_json::from_value(json).unwrap();
        assert!(matches!(parsed.base, Some(BaseSpec::RepoMain)));
    }

    #[test]
    fn delegate_input_with_overlay_deserializes() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
            "base": {
                "kind": "with_overlay",
                "base": { "kind": "branch", "name": "spur/plan-base-xyz" },
                "overlays": [
                    { "source_task_id": "T1", "base_oid": "aaa", "tip_oid": "bbb" }
                ]
            }
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput = serde_json::from_value(json).unwrap();
        match parsed.base {
            Some(BaseSpec::WithOverlay { ref base, ref overlays }) => {
                assert!(matches!(**base, BaseSpec::Branch(ref n) if n == "spur/plan-base-xyz"));
                assert_eq!(overlays.len(), 1);
                assert_eq!(overlays[0].source_task_id, "T1");
            }
            _ => panic!("expected WithOverlay, got {:?}", parsed.base),
        }
    }
}
```

- [ ] **Step 5.2: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml base_spec_tests`
Expected: FAIL — `BaseSpec` does not exist.

- [ ] **Step 5.3: Add `BaseSpec` and `OverlayCommit` to `tools.rs`**

Add near the top of `crates/spur-mcp/src/tools.rs`, after the existing imports:

```rust
use schemars::JsonSchema;

/// Where a worker's worktree should be based.
///
/// Optional on `DelegateToWorkerInput` for backwards compatibility:
/// callers that omit `base` get the legacy behavior (snapshot from
/// repo_root HEAD, equivalent to `BaseSpec::RepoMain`).
///
/// See `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md` §
/// "Companion: BaseSpec (additive API)".
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseSpec {
    /// Snapshot from the orchestrator's repo_root HEAD (legacy default).
    RepoMain,
    /// Branch by name.
    Branch { name: String },
    /// Pinned commit OID.
    Commit { oid: String },
    /// Compose a base by cherry-picking overlay ranges onto another base.
    WithOverlay {
        base: Box<BaseSpec>,
        overlays: Vec<OverlayCommit>,
    },
}

/// One overlay commit range to cherry-pick onto a base.
///
/// `base_oid..tip_oid` is the exclusive-of-base range.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct OverlayCommit {
    /// The plan task whose work this overlay represents (audit / signal context).
    pub source_task_id: String,
    /// Inclusive lower bound (exclusive of base in cherry-pick range).
    pub base_oid: String,
    /// Inclusive upper bound.
    pub tip_oid: String,
}
```

- [ ] **Step 5.4: Add Optional `base` to `DelegationRequest`**

In `crates/spur-mcp/src/tools.rs`, add to the `DelegationRequest` struct (around line 17):

```rust
pub struct DelegationRequest {
    pub id: DelegationId,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    pub respond_to: oneshot::Sender<DelegationResult>,
    pub brain_session_id: spur_acp::BrainSessionId,
    pub delegation_plan: Option<spur_acp::DelegationPlan>,
    pub issue_id: Option<String>,
    pub attempt_tracker: Arc<AtomicU32>,
    /// Where to base the worker's worktree. None → legacy (RepoMain).
    /// Plan-engine dispatches always pass Some(WithOverlay {..}); ad-hoc
    /// brain dispatches may omit. See bd-1dwm design spec.
    pub base: Option<BaseSpec>,
}
```

- [ ] **Step 5.5: Add Optional `base` field to JSON schemas**

In `crates/spur-mcp/src/tool_schemas.rs`, extend both input structs:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateToWorkerInput {
    pub agent: String,
    pub task: String,
    pub context_files: Option<Vec<String>>,
    pub delegation_plan: Option<DelegationPlan>,
    pub issue_id: Option<String>,
    /// Optional explicit worker base. Omit for legacy behavior (RepoMain).
    /// Use `WithOverlay` to apply dependency cherry-picks. See
    /// `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md`.
    pub base: Option<crate::tools::BaseSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelTaskInput {
    pub agent: String,
    pub task: String,
    pub context_files: Option<Vec<String>>,
    pub issue_id: Option<String>,
    pub delegation_plan: Option<DelegationPlan>,
    pub base: Option<crate::tools::BaseSpec>,
}
```

- [ ] **Step 5.6: Update every site that constructs `DelegationRequest` to set `base: None`**

Run: `grep -n 'DelegationRequest {' crates/spur-mcp/src/`
For each construction site, add `base: None,` to the struct literal. (These are non-plan / pre-existing call sites.)

- [ ] **Step 5.7: Run test + check workspace builds**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml base_spec_tests
cargo check --workspace
```
Expected: tests PASS, workspace clean.

- [ ] **Step 5.8: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/tool_schemas.rs crates/spur-mcp/src/
git commit -m "feat(spur-mcp): add BaseSpec + OverlayCommit (additive Optional)

Phase 1 of bd-1dwm. Introduces BaseSpec enum (RepoMain | Branch | Commit
| WithOverlay) and OverlayCommit struct on the delegation request path.
Optional on DelegateToWorkerInput / DelegateParallelTaskInput so existing
brains (which omit base) keep the legacy RepoMain snapshot behavior.

Plan-engine callers (Task 9) will always pass Some(WithOverlay {..}).
Ad-hoc brain dispatches may opt into non-RepoMain bases (deferred to
Phase 2).

Refs: bd-1dwm"
```

---

## Task 6: Persist `dispatched_base_oid` on `PlanTaskEntry` + `AttemptRecord`

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs:96-125` — add field to `AttemptRecord` and `PlanTaskEntry`

- [ ] **Step 6.1: Write the failing test**

Add to `crates/spur-mcp/src/plan/mod.rs` test module:

```rust
#[test]
fn plan_task_entry_serializes_dispatched_base_oid() {
    let entry = super::PlanTaskEntry {
        spec: super::PlanTask {
            task_id: "T1".into(),
            agent: "x".into(),
            task: "do".into(),
            depends_on: vec![],
            issue_id: None,
            context_files: vec![],
        },
        status: super::PlanTaskStatus::Approved { summary: None },
        result: None,
        worker_branch: Some("spur/worker-x-1".into()),
        attempt: 1,
        history: vec![],
        last_delegation_id: None,
        dispatched_base_oid: Some("abc123".into()),
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["dispatched_base_oid"], "abc123");
}

#[test]
fn legacy_plan_task_entry_without_dispatched_base_oid_deserializes() {
    let json = serde_json::json!({
        "spec": { "task_id": "T1", "agent": "x", "task": "do", "depends_on": [], "issue_id": null, "context_files": [] },
        "status": { "status": "approved", "summary": null },
        "result": null,
        "worker_branch": null,
        "attempt": 1,
        "history": [],
        "last_delegation_id": null,
    });
    let entry: super::PlanTaskEntry = serde_json::from_value(json).unwrap();
    assert!(entry.dispatched_base_oid.is_none());
}
```

Note: `PlanTaskEntry` currently derives `Serialize` only, not `Deserialize`. If the second test fails to compile, add `Deserialize` to the derive list (this requires the contained types to also implement it; check the workspace).

If adding `Deserialize` is non-trivial, scope this test to just the serialize path:

```rust
#[test]
fn plan_task_entry_omits_dispatched_base_oid_when_none() {
    let entry = /* ... with dispatched_base_oid: None ... */;
    let json = serde_json::to_value(&entry).unwrap();
    // None should serialize as either absent or null. Accept both.
    assert!(json.get("dispatched_base_oid").map_or(true, |v| v.is_null()));
}
```

- [ ] **Step 6.2: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml plan_task_entry_serializes_dispatched_base_oid`
Expected: FAIL — field does not exist.

- [ ] **Step 6.3: Add fields to `PlanTaskEntry` and `AttemptRecord`**

In `crates/spur-mcp/src/plan/mod.rs`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub worker_branch: Option<String>,
    pub diff_summary: Option<spur_acp::DiffSummary>,
    pub summary: Option<String>,
    pub feedback: String,
    /// HEAD of the worker worktree immediately after overlay cherry-picks
    /// (and before the worker's first commit). Used by `merge_plan` and
    /// `get_task_diff` to compute the worker's net contribution range.
    /// None for legacy attempts dispatched before bd-1dwm Phase 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_base_oid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    pub result: Option<DelegationResult>,
    pub worker_branch: Option<String>,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    #[serde(default)]
    pub history: Vec<AttemptRecord>,
    #[serde(default)]
    pub last_delegation_id: Option<String>,
    /// HEAD of the worker worktree immediately after overlay cherry-picks
    /// for the current (latest) attempt. None for legacy or pre-overlay
    /// dispatches. See bd-1dwm design spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_base_oid: Option<String>,
}
```

- [ ] **Step 6.4: Update every site that constructs `PlanTaskEntry` or `AttemptRecord`**

Run: `grep -rn 'PlanTaskEntry {' crates/spur-mcp/src/`
For each, add `dispatched_base_oid: None,` to the struct literal.

Run: `grep -rn 'AttemptRecord {' crates/spur-mcp/src/`
Same — add `dispatched_base_oid: None,`.

- [ ] **Step 6.5: Run tests**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml plan_task_entry_serializes_dispatched_base_oid
cargo test --manifest-path crates/spur-mcp/Cargo.toml plan
cargo check --workspace
```
Expected: all PASS.

- [ ] **Step 6.6: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): persist dispatched_base_oid on PlanTaskEntry/AttemptRecord

Phase 1 of bd-1dwm. dispatched_base_oid records the worker worktree's
HEAD after overlay cherry-picks (and before the worker's first commit).
Used by merge_plan and get_task_diff to compute the worker's net
contribution range as dispatched_base_oid..worker_branch.

Optional with skip_serializing_if=Option::is_none so legacy entries
without the field continue to round-trip cleanly.

Refs: bd-1dwm"
```

---

## Task 7: Persist `dispatched_base_oid` in audit sentinels + projector

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs`
- Modify: `crates/spur-mcp/src/plan/projector.rs`

The audit sentinel is the durable record of what happened on each attempt; the projector re-hydrates `PlanState` from sentinels on restart. If `dispatched_base_oid` isn't in the sentinel, restart wipes it.

- [ ] **Step 7.1: Locate the sentinel completion struct**

Run: `grep -n 'CompletionAudit\|completion_audit\|kind.*completion' crates/spur-mcp/src/plan/audit_sentinel.rs`
Note the data type that gets serialized as the completion sentinel body.

- [ ] **Step 7.2: Write the failing test**

Add to `crates/spur-mcp/src/plan/audit_sentinel.rs` (in tests):

```rust
#[test]
fn completion_audit_round_trips_dispatched_base_oid() {
    let audit = CompletionAudit { // adapt name to actual struct
        // ... existing fields ...
        dispatched_base_oid: Some("abc123".into()),
        ..Default::default()
    };
    let encoded = encode_completion_audit(&audit);
    let parsed = parse_completion_audit(&encoded).unwrap();
    assert_eq!(parsed.dispatched_base_oid, Some("abc123".into()));
}
```

- [ ] **Step 7.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml completion_audit_round_trips_dispatched_base_oid`
Expected: FAIL.

- [ ] **Step 7.4: Add field + update encoder/parser**

Add `dispatched_base_oid: Option<String>` to the completion-audit struct (with `#[serde(default, skip_serializing_if = "Option::is_none")]`). Update the encoder/parser if they're not derive-driven.

- [ ] **Step 7.5: Update projector to consume the field**

In `crates/spur-mcp/src/plan/projector.rs`, find where completion-audit data is read into `PlanTaskEntry`. Add:

```rust
entry.dispatched_base_oid = parsed_audit.dispatched_base_oid.clone();
// Also propagate into the latest AttemptRecord in entry.history if applicable.
if let Some(latest) = entry.history.last_mut() {
    latest.dispatched_base_oid = parsed_audit.dispatched_base_oid.clone();
}
```

- [ ] **Step 7.6: Run tests + project-replay test**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml plan::audit_sentinel
cargo test --manifest-path crates/spur-mcp/Cargo.toml plan::projector
```
Expected: all PASS.

If a projector replay test exists (e.g., `projector_replays_full_lifecycle`), add a case that asserts `dispatched_base_oid` survives a full round-trip.

- [ ] **Step 7.7: Commit**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs crates/spur-mcp/src/plan/projector.rs
git commit -m "feat(spur-mcp): persist dispatched_base_oid through audit + projector

Phase 1 of bd-1dwm. Without this, dispatched_base_oid is ephemeral —
restart-time projection wipes it and breaks merge_plan / get_task_diff
range computation. Add field to completion-audit sentinel and have
projector hydrate both the entry and the latest AttemptRecord.

Refs: bd-1dwm"
```

---

## Task 8: `WorktreeManager::apply_overlays` + `WorktreeError::OverlayConflict`

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:117-248`

The new method takes a worktree path + a list of `(base_oid, tip_oid)` ranges and runs `git cherry-pick base_oid..tip_oid` for each. On conflict, abort cherry-pick, return structured error with conflicting files.

- [ ] **Step 8.1: Locate the existing `WorktreeError` (or anyhow chain)**

Run: `grep -n 'WorktreeError\|enum.*Error' crates/spur-worktree/src/manager.rs`
Note the error pattern.

If `WorktreeError` doesn't exist (the file uses `anyhow::Result<>`), introduce a structured `WorktreeError` enum here:

```rust
#[derive(Debug)]
pub enum WorktreeError {
    Anyhow(anyhow::Error),
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anyhow(e) => write!(f, "{e}"),
            Self::OverlayConflict { source_task_id, files } => {
                write!(f, "overlay cherry-pick conflict applying {source_task_id}: {} files", files.len())
            }
        }
    }
}

impl std::error::Error for WorktreeError {}

impl From<anyhow::Error> for WorktreeError {
    fn from(e: anyhow::Error) -> Self { Self::Anyhow(e) }
}
```

- [ ] **Step 8.2: Write the failing test**

Add to `crates/spur-worktree/src/manager.rs` (or a sibling test file):

```rust
#[cfg(test)]
mod overlay_tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = StdCommand::new("git").args(args).current_dir(repo).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README"), "init\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn apply_overlays_clean_cherry_picks() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        // Create upstream task branch with one commit adding foo.rs.
        run_git(dir.path(), &["checkout", "-q", "-B", "task1", "main"]);
        std::fs::write(dir.path().join("foo.rs"), "// foo\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);
        let task1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        // Create worker worktree from main.
        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(dir.path(), &["worktree", "add", worker_path.to_str().unwrap(), "-b", "worker1", "main"]);

        // Apply overlay [task1].
        let overlays = vec![(main_oid.clone(), task1_tip.clone())];
        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        mgr.apply_overlays(&worker_path, &[("task1".into(), main_oid, task1_tip)]).await
            .expect("clean cherry-pick should succeed");

        // Verify foo.rs is present in worker worktree.
        assert!(worker_path.join("foo.rs").exists(), "overlay should have brought foo.rs into worker worktree");
    }

    #[tokio::test]
    async fn apply_overlays_returns_overlay_conflict_on_conflict() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        // Two parallel branches both modify same file differently.
        std::fs::write(dir.path().join("foo.rs"), "shared\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "shared base"]);
        let base_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task1", &base_oid]);
        std::fs::write(dir.path().join("foo.rs"), "task1 version\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);
        let task1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task2", &base_oid]);
        std::fs::write(dir.path().join("foo.rs"), "task2 version\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task2"]);
        let task2_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        // Worker worktree starts from task1; try to overlay task2 → conflict.
        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(dir.path(), &["worktree", "add", worker_path.to_str().unwrap(), "-b", "worker1", "task1"]);

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        let result = mgr.apply_overlays(&worker_path, &[("task2".into(), base_oid, task2_tip)]).await;
        match result {
            Err(WorktreeError::OverlayConflict { source_task_id, files }) => {
                assert_eq!(source_task_id, "task2");
                assert!(files.iter().any(|f| f == "foo.rs"), "expected foo.rs in conflict files: {files:?}");
            }
            other => panic!("expected OverlayConflict, got {other:?}"),
        }
    }
}
```

- [ ] **Step 8.3: Run tests to verify they fail**

Run: `cargo test --manifest-path crates/spur-worktree/Cargo.toml overlay_tests`
Expected: FAIL — `apply_overlays` does not exist.

- [ ] **Step 8.4: Implement `apply_overlays`**

Add to `crates/spur-worktree/src/manager.rs`:

```rust
impl WorktreeManager {
    /// Apply a chain of overlay cherry-picks to a worker worktree.
    ///
    /// Each overlay is `(source_task_id, base_oid, tip_oid)`. Runs
    /// `git cherry-pick base_oid..tip_oid` in `worktree_path` for each.
    /// On conflict: abort cherry-pick, return structured error with
    /// the conflicting task id and file list.
    pub async fn apply_overlays(
        &self,
        worktree_path: &Path,
        overlays: &[(String, String, String)],
    ) -> Result<(), WorktreeError> {
        for (source_task_id, base_oid, tip_oid) in overlays {
            let range = format!("{base_oid}..{tip_oid}");
            let pick_result = self
                .run_git(&["cherry-pick", &range], Some(worktree_path))
                .await;
            if let Err(_e) = pick_result {
                // Capture conflicting files before aborting.
                let conflict_files = self
                    .run_git(&["diff", "--name-only", "--diff-filter=U"], Some(worktree_path))
                    .await
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let _ = self
                    .run_git(&["cherry-pick", "--abort"], Some(worktree_path))
                    .await;
                return Err(WorktreeError::OverlayConflict {
                    source_task_id: source_task_id.clone(),
                    files: conflict_files,
                });
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 8.5: Run tests to verify pass**

Run: `cargo test --manifest-path crates/spur-worktree/Cargo.toml overlay_tests`
Expected: PASS (2/2).

- [ ] **Step 8.6: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-worktree): apply_overlays + OverlayConflict error

Phase 1 of bd-1dwm. apply_overlays takes a list of cherry-pick ranges
and applies them in order to a worker worktree. On conflict: abort
cherry-pick, return WorktreeError::OverlayConflict with source task id
+ conflicting file list. Covered by two unit tests (clean apply +
conflict path).

Refs: bd-1dwm"
```

---

## Task 9: Wire `BaseSpec` through orchestrator dispatch

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:6263-6318`

Resolve the `BaseSpec` to a base ref at dispatch, pass it to `create_worktree`, then call `apply_overlays` for `WithOverlay` cases. Record `dispatched_base_oid` after overlays succeed.

- [ ] **Step 9.1: Add base resolution helper**

Near the top of `crates/spur-core/src/orchestrator.rs` (or in a new `base_spec.rs` sibling module), add:

```rust
use spur_mcp::tools::BaseSpec;

/// Resolve a `BaseSpec` into a concrete branch name to pass into
/// `WorktreeManager::create_worktree`. For `WithOverlay`, this returns
/// the base of the overlay chain — overlays are applied separately
/// after worktree creation.
fn resolve_base_branch(spec: &BaseSpec, snapshot_branch: &str) -> String {
    match spec {
        BaseSpec::RepoMain => snapshot_branch.to_string(),
        BaseSpec::Branch { name } => name.clone(),
        BaseSpec::Commit { oid } => oid.clone(),
        BaseSpec::WithOverlay { base, .. } => resolve_base_branch(base, snapshot_branch),
    }
}

/// Extract the overlay list from a BaseSpec (empty if not WithOverlay).
fn extract_overlays(spec: &BaseSpec) -> Vec<(String, String, String)> {
    match spec {
        BaseSpec::WithOverlay { overlays, .. } => overlays
            .iter()
            .map(|o| (o.source_task_id.clone(), o.base_oid.clone(), o.tip_oid.clone()))
            .collect(),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 9.2: Add `OverlayConflict` to `AttemptSetupError`**

In `crates/spur-core/src/orchestrator.rs`, find the `AttemptSetupError` enum (search `enum AttemptSetupError`). Add:

```rust
#[derive(Debug)]
pub enum AttemptSetupError {
    // ... existing variants ...
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
}

impl std::fmt::Display for AttemptSetupError {
    // Add the new arm:
    // Self::OverlayConflict { source_task_id, files } => {
    //     write!(f, "overlay conflict applying {source_task_id}: {} files", files.len())
    // }
}
```

- [ ] **Step 9.3: Modify `run_one_worker_attempt` to apply overlays**

In `crates/spur-core/src/orchestrator.rs:6310-6318`, replace the worktree-creation block with:

```rust
// 1. Snapshot brain state and create worktree.
let snapshot_branch = worktrees
    .snapshot_brain_state()
    .await
    .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?;

// 1a. Resolve BaseSpec → concrete base branch (defaults to snapshot for legacy).
let base_branch = match &ctx.base {
    Some(spec) => resolve_base_branch(spec, &snapshot_branch),
    None => snapshot_branch.clone(),
};

let worktree_info = worktrees
    .create_worktree(&worker_session, ctx.agent, &base_branch)
    .await
    .map_err(|e| AttemptSetupError::WorktreeFailed(e.to_string()))?;

// 1b. Apply overlays inside the worker worktree (G-strict).
let overlays = ctx.base.as_ref().map(extract_overlays).unwrap_or_default();
if !overlays.is_empty() {
    if let Err(e) = worktrees.apply_overlays(&worktree_info.path, &overlays).await {
        // Cleanup partial worktree before propagating.
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(match e {
            spur_worktree::manager::WorktreeError::OverlayConflict { source_task_id, files } => {
                AttemptSetupError::OverlayConflict { source_task_id, files }
            }
            other => AttemptSetupError::WorktreeFailed(other.to_string()),
        });
    }
}

// 1c. Record dispatched_base_oid (worktree HEAD post-overlay, pre-worker).
let dispatched_base_oid = worktrees
    .resolve_head(&worktree_info.path)
    .await
    .map_err(|e| AttemptSetupError::WorktreeFailed(format!("resolve worktree HEAD: {e}")))?;
ctx.record_dispatched_base_oid(dispatched_base_oid.clone()); // see Step 9.4 for plumbing
```

- [ ] **Step 9.4: Add `base` field to `WorkerAttemptCtx` + plumb it**

In `crates/spur-core/src/orchestrator.rs`, find `struct WorkerAttemptCtx<'a>` (around line 6263) and add:

```rust
struct WorkerAttemptCtx<'a> {
    // ... existing fields ...
    base: Option<spur_mcp::tools::BaseSpec>,
    /// Sender used to propagate dispatched_base_oid back to the
    /// reconciler so it can persist on PlanTaskEntry.
    dispatched_base_oid_tx: Option<tokio::sync::oneshot::Sender<String>>,
}
```

Update every construction site of `WorkerAttemptCtx` (search `WorkerAttemptCtx {` in the same file) to set both fields. The `base` is read from the incoming `DelegationRequest.base`; the `dispatched_base_oid_tx` is wired up by the reconciler dispatch path (Task 10).

- [ ] **Step 9.5: Add `WorktreeManager::resolve_head` helper if missing**

In `crates/spur-worktree/src/manager.rs`:

```rust
impl WorktreeManager {
    /// Resolve HEAD of the given worktree path to its OID.
    pub async fn resolve_head(&self, worktree_path: &Path) -> Result<String> {
        self.run_git(&["rev-parse", "HEAD"], Some(worktree_path)).await
    }
}
```

- [ ] **Step 9.6: Build check**

Run: `cargo check --workspace`
Expected: clean.

- [ ] **Step 9.7: Add a unit test for `resolve_base_branch` + `extract_overlays`**

Add to `crates/spur-core/src/orchestrator.rs` test module:

```rust
#[test]
fn resolve_base_branch_unwraps_with_overlay() {
    let spec = BaseSpec::WithOverlay {
        base: Box::new(BaseSpec::Branch { name: "spur/plan-base-xyz".into() }),
        overlays: vec![],
    };
    assert_eq!(resolve_base_branch(&spec, "fallback"), "spur/plan-base-xyz");
}

#[test]
fn resolve_base_branch_falls_back_for_repo_main() {
    let spec = BaseSpec::RepoMain;
    assert_eq!(resolve_base_branch(&spec, "spur/brain-snapshot-X"), "spur/brain-snapshot-X");
}

#[test]
fn extract_overlays_returns_empty_for_non_overlay() {
    assert!(extract_overlays(&BaseSpec::RepoMain).is_empty());
    assert!(extract_overlays(&BaseSpec::Branch { name: "x".into() }).is_empty());
}

#[test]
fn extract_overlays_returns_all_for_with_overlay() {
    let spec = BaseSpec::WithOverlay {
        base: Box::new(BaseSpec::RepoMain),
        overlays: vec![
            spur_mcp::tools::OverlayCommit { source_task_id: "T1".into(), base_oid: "a".into(), tip_oid: "b".into() },
            spur_mcp::tools::OverlayCommit { source_task_id: "T2".into(), base_oid: "b".into(), tip_oid: "c".into() },
        ],
    };
    let overlays = extract_overlays(&spec);
    assert_eq!(overlays.len(), 2);
    assert_eq!(overlays[0].0, "T1");
    assert_eq!(overlays[1].0, "T2");
}
```

Run: `cargo test --manifest-path crates/spur-core/Cargo.toml resolve_base_branch`
Expected: PASS.

- [ ] **Step 9.8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-core): wire BaseSpec through orchestrator dispatch

Phase 1 of bd-1dwm. run_one_worker_attempt now:
- resolves BaseSpec → base branch (or falls back to snapshot for legacy)
- creates worktree from that base
- applies overlays (cherry-picks dep ranges) in the worker worktree
- records dispatched_base_oid (= worktree HEAD post-overlay)
- propagates OverlayConflict as AttemptSetupError variant

Legacy path (None base) preserves today's snapshot-from-HEAD behavior.

Refs: bd-1dwm"
```

---

## Task 10: Reconciler computes overlay closure for plan tasks

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs:625-656`
- Modify: `crates/spur-mcp/src/plan/mod.rs` (add helper for transitive closure)

The reconciler walks `depends_on` transitively over `Approved` tasks and constructs a `WithOverlay` `BaseSpec` for the dispatch.

- [ ] **Step 10.1: Add transitive closure helper to `plan/mod.rs`**

```rust
impl PlanState {
    /// Compute the topologically-ordered transitive closure of `task_id`'s
    /// dependencies, restricted to tasks currently in Approved status.
    /// Returns dep entries in dispatch order (deepest first).
    pub fn approved_dep_closure(&self, task_id: &str) -> Vec<&PlanTaskEntry> {
        let mut visited = std::collections::HashSet::new();
        let mut order = Vec::new();
        self.dfs_approved(task_id, &mut visited, &mut order);
        order
            .iter()
            .filter_map(|tid| self.tasks.iter().find(|e| &e.spec.task_id == tid))
            .collect()
    }

    fn dfs_approved(
        &self,
        task_id: &str,
        visited: &mut std::collections::HashSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(task_id.to_string()) {
            return;
        }
        if let Some(entry) = self.tasks.iter().find(|e| e.spec.task_id == task_id) {
            for dep in &entry.spec.depends_on {
                self.dfs_approved(dep, visited, order);
            }
            // Only include the task itself if it's Approved AND it's not the entry-point task.
            // The entry point is the task being dispatched; skip self.
            if matches!(entry.status, PlanTaskStatus::Approved { .. })
                && order.iter().all(|t| t != task_id)
            {
                order.push(task_id.to_string());
            }
        }
    }
}
```

- [ ] **Step 10.2: Write test for closure helper**

```rust
#[test]
fn approved_dep_closure_returns_topo_order() {
    // Plan: T3 depends on [T1, T2]; T1 and T2 both depend on root (T0).
    // T0, T1 approved; T2 still pending; T3 dispatching.
    let state = PlanState {
        plan_id: "p".into(),
        tasks: vec![
            entry("T0", &[], PlanTaskStatus::Approved { summary: None }),
            entry("T1", &["T0"], PlanTaskStatus::Approved { summary: None }),
            entry("T2", &["T0"], PlanTaskStatus::Pending),
            entry("T3", &["T1", "T2"], PlanTaskStatus::Ready),
        ],
        // ... fill other PlanState fields with defaults ...
        brain_session_id: BrainSessionId::new(spur_acp::SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
    };

    let closure = state.approved_dep_closure("T3");
    let ids: Vec<&str> = closure.iter().map(|e| e.spec.task_id.as_str()).collect();

    // T2 not Approved, so excluded. T0 should come before T1 (topo).
    // T3 itself excluded (entry point).
    assert_eq!(ids, vec!["T0", "T1"]);
}

fn entry(id: &str, deps: &[&str], status: PlanTaskStatus) -> PlanTaskEntry {
    PlanTaskEntry {
        spec: PlanTask {
            task_id: id.into(),
            agent: "x".into(),
            task: "do".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            context_files: vec![],
        },
        status,
        result: None,
        worker_branch: Some(format!("spur/worker-x-{id}")),
        attempt: 1,
        history: vec![],
        last_delegation_id: None,
        dispatched_base_oid: Some(format!("{id}-base")),
    }
}
```

- [ ] **Step 10.3: Run tests**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml approved_dep_closure_returns_topo_order
```
Expected: PASS.

- [ ] **Step 10.4: Wire closure into reconciler dispatch path**

In `crates/spur-mcp/src/plan/reconciler.rs`, find where the reconciler dispatches a Ready task (around line 625-656). Build the `BaseSpec`:

```rust
use spur_mcp::tools::{BaseSpec, OverlayCommit};

// Compute overlay from approved deps.
let dep_closure = plan_state.approved_dep_closure(&task_id);
let overlays: Vec<OverlayCommit> = dep_closure
    .iter()
    .filter_map(|dep| {
        let base_oid = dep.dispatched_base_oid.clone()?;
        let worker_branch = dep.worker_branch.as_ref()?.clone();
        // Resolve worker_branch tip OID via git rev-parse.
        let tip_oid = git_rev_parse(repo_root, &worker_branch).ok()?;
        Some(OverlayCommit {
            source_task_id: dep.spec.task_id.clone(),
            base_oid,
            tip_oid,
        })
    })
    .collect();

let base_spec = BaseSpec::WithOverlay {
    base: Box::new(BaseSpec::Branch {
        name: plan_state.base_snapshot_branch.clone().unwrap_or_else(|| "HEAD".into()),
    }),
    overlays,
};

// Pass base_spec into the DelegationRequest construction.
let request = DelegationRequest {
    // ... existing fields ...
    base: Some(base_spec),
    // ... rest ...
};
```

Add a `git_rev_parse` helper if one isn't already in scope. Look in `crates/spur-mcp/src/server.rs` or `crates/spur-worktree/src/manager.rs` for an existing `run_git_capture` / `rev-parse` utility to reuse.

- [ ] **Step 10.5: Capture `dispatched_base_oid` from orchestrator response**

The orchestrator records `dispatched_base_oid` per attempt (Task 9 wired the `dispatched_base_oid_tx` oneshot). The reconciler needs to receive it and persist on `PlanTaskEntry`. Add to the dispatch flow:

```rust
let (oid_tx, oid_rx) = tokio::sync::oneshot::channel();
let request = DelegationRequest {
    // ... base, dispatched_base_oid_tx: Some(oid_tx), ...
};
// dispatch and await the response as today
let response = orchestrator.dispatch(request).await?;

// Capture dispatched_base_oid (best-effort; may not arrive if pre-overlay error).
if let Ok(oid) = oid_rx.try_recv() {
    plan_state.set_dispatched_base_oid(&task_id, oid);
}
```

- [ ] **Step 10.6: Run reconciler tests**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml plan::reconciler
cargo check --workspace
```
Expected: all PASS, workspace clean.

- [ ] **Step 10.7: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/plan/reconciler.rs
git commit -m "feat(spur-mcp): reconciler computes overlay closure for plan dispatch

Phase 1 of bd-1dwm. Adds approved_dep_closure helper that returns the
topologically-ordered transitive closure of a task's depends_on edges,
restricted to currently-Approved tasks. Reconciler builds a WithOverlay
BaseSpec and passes it as DelegationRequest.base for plan tasks.

dispatched_base_oid is captured from the orchestrator via a oneshot
channel and persisted on PlanTaskEntry.

Refs: bd-1dwm"
```

---

## Task 11: New `BlockedOnSetupConflict` plan task status + reconciler routing

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs:54-78` — add status variant
- Modify: `crates/spur-mcp/src/plan/reconciler.rs` — translate `OverlayConflict` to status + signal

- [ ] **Step 11.1: Add status variant**

In `crates/spur-mcp/src/plan/mod.rs`, extend `PlanTaskStatus`:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanTaskStatus {
    Pending,
    Ready,
    Dispatched { delegation_id: String },
    AwaitingReview { summary: Option<String> },
    Approved { summary: Option<String> },
    Rejected { feedback: Option<String> },
    Failed { error: String },
    Cancelled { reason: String },
    Superseded { mutation_id: String, by: Vec<String> },
    /// Setup-time overlay conflict: dispatch could not start because
    /// cherry-picking a dependency's range onto the worker worktree
    /// produced a merge conflict. Brain must resolve (introduce a
    /// merge task, re-spec downstream, or abort plan) before retry.
    /// See bd-1dwm design spec.
    BlockedOnSetupConflict {
        dep_task_id: String,
        files: Vec<String>,
    },
}

impl PlanTaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Approved { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::Superseded { .. }
            // BlockedOnSetupConflict is NOT terminal — brain resolves and retries.
        )
    }
}
```

- [ ] **Step 11.2: Add a `signal:integration-conflict` label constant**

Wherever signal label constants live (search for `"signal:scope-drift"` to find the file):

```rust
pub const SIGNAL_LABEL_INTEGRATION_CONFLICT: &str = "signal:integration-conflict";
pub const SIGNAL_LABEL_POTENTIAL_CLOBBER: &str = "signal:potential-clobber";
```

- [ ] **Step 11.3: Wire `OverlayConflict` routing in reconciler**

In `crates/spur-mcp/src/plan/reconciler.rs`, where dispatch errors are handled (search for `AttemptSetupError`):

```rust
use spur_acp::AttemptSetupError;

match dispatch_result {
    Ok(outcome) => { /* existing handling */ }
    Err(AttemptSetupError::OverlayConflict { source_task_id, files }) => {
        // Write sentinel comment.
        let comment = format!(
            "[[spur-signal v1]]\n{}",
            serde_json::to_string(&serde_json::json!({
                "kind": "integration_conflict",
                "dep_task_id": &source_task_id,
                "files": &files,
            })).unwrap()
        );
        backend.update_issue_comment(&issue_id, &comment).await?;
        backend.update_issue_labels(&issue_id, &[SIGNAL_LABEL_INTEGRATION_CONFLICT], &[]).await?;

        // Set plan task status (issue itself stays Open).
        plan_state.set_status(&task_id, PlanTaskStatus::BlockedOnSetupConflict {
            dep_task_id: source_task_id,
            files,
        });
    }
    Err(other) => { /* existing fail-handling */ }
}
```

- [ ] **Step 11.4: Write test for the routing**

In a reconciler test file (or `crates/spur-mcp/tests/`):

```rust
#[tokio::test]
async fn overlay_conflict_routes_to_blocked_on_setup_conflict() {
    let harness = ReconcilerTestHarness::new().await;
    // Configure a stub orchestrator that returns OverlayConflict on first dispatch.
    harness.stub_orchestrator(|_req| {
        Err(AttemptSetupError::OverlayConflict {
            source_task_id: "T1".into(),
            files: vec!["foo.rs".into()],
        })
    });
    let plan_id = harness.submit_plan(/* ... 2-task plan ... */).await;
    harness.tick_reconciler().await;

    let entry = harness.get_task("T2").await;
    assert!(matches!(
        entry.status,
        PlanTaskStatus::BlockedOnSetupConflict { ref dep_task_id, .. } if dep_task_id == "T1"
    ));
    let labels = harness.get_issue_labels("T2").await;
    assert!(labels.iter().any(|l| l == "signal:integration-conflict"));
}
```

(Adapt `ReconcilerTestHarness` to existing test infrastructure.)

- [ ] **Step 11.5: Run tests + workspace check**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml overlay_conflict_routes
cargo check --workspace
```
Expected: PASS, clean.

- [ ] **Step 11.6: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/plan/reconciler.rs
git commit -m "feat(spur-mcp): BlockedOnSetupConflict status + signal routing

Phase 1 of bd-1dwm. New PlanTaskStatus::BlockedOnSetupConflict variant
distinct from Failed (issue stays Open, brain can retry after resolving
upstream conflict). Reconciler translates AttemptSetupError::OverlayConflict
into:
- sentinel comment (kind=integration_conflict)
- signal:integration-conflict label on the issue
- BlockedOnSetupConflict plan task status

Refs: bd-1dwm"
```

---

## Task 12: TUI surfaces `BlockedOnSetupConflict` distinct from `Failed`

**Files:**
- Modify: `crates/spur-tui/src/components/plan_pulse.rs`

- [ ] **Step 12.1: Locate the status-rendering site**

Run: `grep -n 'PlanTaskStatus\|Failed\|status_badge' crates/spur-tui/src/components/plan_pulse.rs | head -10`

- [ ] **Step 12.2: Write the failing test**

```rust
#[test]
fn plan_pulse_renders_blocked_on_setup_conflict_distinct_from_failed() {
    let task_blocked = TaskRow {
        task_id: "T2".into(),
        status: TaskStatus::BlockedOnSetupConflict {
            dep_task_id: "T1".into(),
            files: vec!["foo.rs".into()],
        },
        // ... other fields ...
    };
    let task_failed = TaskRow {
        task_id: "T3".into(),
        status: TaskStatus::Failed { error: "x".into() },
        // ... other fields ...
    };
    let blocked_render = render_task_row(&task_blocked);
    let failed_render = render_task_row(&task_failed);

    assert!(blocked_render.contains("⛔ BLOCKED") || blocked_render.contains("conflict"));
    assert!(failed_render.contains("FAILED"));
    assert_ne!(blocked_render, failed_render);
}
```

- [ ] **Step 12.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-tui/Cargo.toml plan_pulse_renders_blocked_on_setup_conflict_distinct_from_failed`
Expected: FAIL.

- [ ] **Step 12.4: Add the rendering**

In `crates/spur-tui/src/components/plan_pulse.rs`, add a match arm in the status-rendering function:

```rust
PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
    spans.push(Span::styled(
        format!(" ⛔ BLOCKED ({} files conflict with {}) ", files.len(), dep_task_id),
        Style::default().fg(Color::White).bg(Color::Red),
    ));
}
```

- [ ] **Step 12.5: Run test**

Run: `cargo test --manifest-path crates/spur-tui/Cargo.toml plan_pulse_renders_blocked_on_setup_conflict_distinct_from_failed`
Expected: PASS.

- [ ] **Step 12.6: Commit**

```bash
git add crates/spur-tui/src/components/plan_pulse.rs
git commit -m "feat(spur-tui): render BlockedOnSetupConflict distinct from Failed

Phase 1 of bd-1dwm. New plan task status gets its own visual treatment
(red BLOCKED badge with conflict file count + dep id) so the operator
doesn't conflate setup-conflict (recoverable, brain decides) with
worker failure (typically terminal).

Refs: bd-1dwm"
```

---

## Task 13: `preview_task_base` MCP tool

**Files:**
- Modify: `crates/spur-mcp/src/tool_schemas.rs` — add input/output schemas
- Modify: `crates/spur-mcp/src/tools.rs` — add tool definition
- Modify: `crates/spur-mcp/src/server.rs` — add handler

Returns the computed overlay + predicted base OID for a given (plan, task) WITHOUT creating a real worktree. Reuses `WorktreeManager::apply_overlays` against an ephemeral throwaway worktree.

- [ ] **Step 13.1: Define input + output schemas**

In `crates/spur-mcp/src/tool_schemas.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewTaskBaseInput {
    pub plan_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewTaskBaseOutput {
    pub overlays: Vec<crate::tools::OverlayCommit>,
    /// HEAD after overlays applied, if clean. None if conflict.
    pub predicted_base_oid: Option<String>,
    pub conflict: Option<PreviewConflict>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewConflict {
    pub dep_task_id: String,
    pub files: Vec<String>,
}
```

- [ ] **Step 13.2: Add tool definition**

In `crates/spur-mcp/src/tools.rs`, add:

```rust
fn preview_task_base_def() -> ToolDefinition {
    ToolDefinition {
        name: "preview_task_base".into(),
        description: "Read-only: returns the overlay commits and predicted base OID for a given plan task without creating a worktree. Use this BEFORE approving a downstream task to surface integration conflicts early. Returns null `predicted_base_oid` and a `conflict` payload when overlays cannot be applied cleanly.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PreviewTaskBaseInput>(),
    }
}
```

Add `preview_task_base_def()` to the list of tools returned by `tools/list` (search for `delegate_to_worker_def()` to find the registration site).

- [ ] **Step 13.3: Implement the handler**

In `crates/spur-mcp/src/server.rs`, find the tool dispatch (search for `"delegate_to_worker"` match arm). Add:

```rust
"preview_task_base" => {
    let input: crate::tool_schemas::PreviewTaskBaseInput = serde_json::from_value(args)?;
    let plan_state = self.plan_engine.get_state(&input.plan_id).await?;

    // Compute overlays via the same helper Task 10 added.
    let dep_closure = plan_state.approved_dep_closure(&input.task_id);
    let overlays: Vec<crate::tools::OverlayCommit> = dep_closure
        .iter()
        .filter_map(|dep| {
            let base_oid = dep.dispatched_base_oid.clone()?;
            let worker_branch = dep.worker_branch.as_ref()?.clone();
            let tip_oid = git_rev_parse(&self.repo_root, &worker_branch).ok()?;
            Some(crate::tools::OverlayCommit {
                source_task_id: dep.spec.task_id.clone(),
                base_oid,
                tip_oid,
            })
        })
        .collect();

    // Dry-run: create throwaway worktree, apply overlays, capture result, remove worktree.
    let throwaway_id = format!("preview-{}", uuid::Uuid::new_v4());
    let throwaway_path = self.repo_root.join(".spur/worktrees/preview").join(&throwaway_id);
    std::fs::create_dir_all(throwaway_path.parent().unwrap())?;

    let base_branch = plan_state.base_snapshot_branch.clone().unwrap_or_else(|| "HEAD".into());
    let throwaway_branch = format!("spur/preview-{throwaway_id}");

    self.worktree_mgr.create_worktree_at(&throwaway_path, &throwaway_branch, &base_branch).await?;

    let overlay_args: Vec<(String, String, String)> = overlays
        .iter()
        .map(|o| (o.source_task_id.clone(), o.base_oid.clone(), o.tip_oid.clone()))
        .collect();

    let result = self.worktree_mgr.apply_overlays(&throwaway_path, &overlay_args).await;

    let output = match result {
        Ok(()) => {
            let head = self.worktree_mgr.resolve_head(&throwaway_path).await.ok();
            crate::tool_schemas::PreviewTaskBaseOutput {
                overlays,
                predicted_base_oid: head,
                conflict: None,
            }
        }
        Err(spur_worktree::manager::WorktreeError::OverlayConflict { source_task_id, files }) => {
            crate::tool_schemas::PreviewTaskBaseOutput {
                overlays,
                predicted_base_oid: None,
                conflict: Some(crate::tool_schemas::PreviewConflict {
                    dep_task_id: source_task_id,
                    files,
                }),
            }
        }
        Err(other) => return Err(anyhow::anyhow!("preview overlay failed: {other}")),
    };

    // Cleanup: remove worktree + branch.
    let _ = self.worktree_mgr.remove_worktree_at(&throwaway_path).await;
    let _ = self.worktree_mgr.delete_branch(&throwaway_branch).await;

    Ok(serde_json::to_value(output)?)
}
```

(`create_worktree_at`, `remove_worktree_at`, and `delete_branch` may need to be added as new helpers on `WorktreeManager` if they don't exist. They're thin wrappers over `git worktree add` / `git worktree remove` / `git branch -D`.)

- [ ] **Step 13.4: Write integration test**

```rust
#[tokio::test]
async fn preview_task_base_returns_overlays_and_base_oid_when_clean() {
    let harness = TestHarness::new().await;
    let plan_id = harness.submit_plan(/* 2-task plan, T1 → T2 */).await;
    harness.dispatch_and_approve("T1", &[("foo.rs", "// t1\n")]).await;

    let result: PreviewTaskBaseOutput = harness.call_tool("preview_task_base", serde_json::json!({
        "plan_id": plan_id,
        "task_id": "T2",
    })).await.unwrap();

    assert_eq!(result.overlays.len(), 1);
    assert_eq!(result.overlays[0].source_task_id, "T1");
    assert!(result.predicted_base_oid.is_some());
    assert!(result.conflict.is_none());
}

#[tokio::test]
async fn preview_task_base_reports_conflict_when_overlays_collide() {
    // Use a plan where T2 has 2 deps that conflict on the same file.
    let harness = TestHarness::new().await;
    // ... build plan with T1, T2 both modifying foo.rs; T3 depends on both ...
    harness.dispatch_and_approve("T1", &[("foo.rs", "T1\n")]).await;
    harness.dispatch_and_approve("T2", &[("foo.rs", "T2\n")]).await;

    let result: PreviewTaskBaseOutput = harness.call_tool("preview_task_base", serde_json::json!({
        "plan_id": plan_id,
        "task_id": "T3",
    })).await.unwrap();

    assert_eq!(result.overlays.len(), 2);
    assert!(result.predicted_base_oid.is_none());
    assert!(result.conflict.is_some());
    let c = result.conflict.unwrap();
    assert_eq!(c.dep_task_id, "T2"); // T1 applies clean; T2 conflicts
    assert!(c.files.iter().any(|f| f == "foo.rs"));
}
```

- [ ] **Step 13.5: Run tests**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml preview_task_base
cargo check --workspace
```
Expected: PASS.

- [ ] **Step 13.6: Commit**

```bash
git add crates/spur-mcp/src/tool_schemas.rs crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-mcp): preview_task_base MCP tool

Phase 1 of bd-1dwm. Read-only dry-run handler that returns the overlay
commits and predicted base OID for a given (plan_id, task_id) without
creating a real worker worktree. Brain calls this before approving a
downstream task to surface integration conflicts early.

Conflict path: returns null predicted_base_oid + structured conflict
(dep_task_id, files). Same shape as the live OverlayConflict error.

Refs: bd-1dwm"
```

---

## Task 14: Fix `get_task_diff` to use `dispatched_base_oid`

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:4429-4446`

- [ ] **Step 14.1: Locate the function**

Run: `grep -n 'fn get_task_diff\|"get_task_diff"\|base_snapshot..worker_branch' crates/spur-mcp/src/server.rs`

- [ ] **Step 14.2: Write a failing test**

```rust
#[tokio::test]
async fn get_task_diff_uses_dispatched_base_oid_when_present() {
    let harness = TestHarness::new().await;
    let plan_id = harness.submit_plan(/* 2-task plan T1 → T2 */).await;

    // T1 creates foo.rs.
    harness.dispatch_and_approve("T1", &[("foo.rs", "T1\n")]).await;

    // T2 modifies bar.rs (does NOT touch foo.rs even though it inherits via overlay).
    harness.dispatch_and_approve("T2", &[("bar.rs", "T2\n")]).await;

    // get_task_diff for T2 should ONLY contain bar.rs, not foo.rs (which was inherited via overlay).
    let diff: String = harness.call_tool("get_task_diff", serde_json::json!({
        "plan_id": plan_id,
        "task_id": "T2",
    })).await.unwrap();

    assert!(diff.contains("bar.rs"), "T2 diff should include bar.rs: {diff}");
    assert!(!diff.contains("foo.rs"), "T2 diff must NOT include inherited foo.rs: {diff}");
}
```

- [ ] **Step 14.3: Run test to verify it fails**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml get_task_diff_uses_dispatched_base_oid`
Expected: FAIL — diff includes foo.rs.

- [ ] **Step 14.4: Apply the fix**

In the `get_task_diff` handler, change the diff range computation from `base_snapshot..worker_branch` to:

```rust
let base = entry.dispatched_base_oid
    .as_deref()
    .unwrap_or(plan_state.base_snapshot_branch.as_deref().unwrap_or("HEAD"));
let range = format!("{base}..{worker_branch}");
let diff = run_git_capture(repo_root, None, &["diff", &range]).await?;
```

If `dispatched_base_oid` is None (legacy task), fall back to the old behavior with a `tracing::warn!` so we know when fallback fires.

- [ ] **Step 14.5: Run test**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml get_task_diff_uses_dispatched_base_oid`
Expected: PASS.

- [ ] **Step 14.6: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "fix(spur-mcp): get_task_diff uses dispatched_base_oid range

Phase 1 of bd-1dwm. Without this fix, overlay commits inherited from
upstream tasks pollute the per-task review diff (codex review caught
this regression at server.rs:4429-4446). Use dispatched_base_oid..tip
for the worker's net contribution; fall back to base_snapshot..tip with
a warn for legacy tasks lacking the field.

Refs: bd-1dwm"
```

---

## Task 15: `DispatchOverlayApplied` lineage event

**Files:**
- Modify: `crates/spur-acp/src/event.rs` (or wherever `SpurEventBody` lives) — add variant
- Modify: `crates/spur-core/src/orchestrator.rs:6298-6305` — emit before agent init
- Modify: `crates/spur-core/src/lineage/adapter.rs:123-190` — handle new event

- [ ] **Step 15.1: Locate `SpurEventBody`**

Run: `grep -rn 'pub enum SpurEventBody' crates/spur-acp/src/`

- [ ] **Step 15.2: Add the variant**

```rust
pub enum SpurEventBody {
    // ... existing variants ...
    DispatchOverlayApplied {
        request_id: String,
        base_spec: spur_mcp::tools::BaseSpec,
        dispatched_base_oid: String,
        overlay_task_ids: Vec<String>,
    },
}
```

(If `spur-acp` cannot depend on `spur-mcp` due to crate ordering, define a sibling `BaseSpecLineage` newtype in `spur-acp` that mirrors the structure. Or use `serde_json::Value` to carry the spec.)

- [ ] **Step 15.3: Emit the event in orchestrator**

In `crates/spur-core/src/orchestrator.rs`, immediately after `dispatched_base_oid` is recorded (Step 9.3), before `connection.initialize`:

```rust
funnel.emit(SpurEventBody::DispatchOverlayApplied {
    request_id: ctx.request_id.to_string(),
    base_spec: ctx.base.clone().unwrap_or(BaseSpec::RepoMain),
    dispatched_base_oid: dispatched_base_oid.clone(),
    overlay_task_ids: overlays.iter().map(|(id, _, _)| id.clone()).collect(),
});
```

- [ ] **Step 15.4: Add adapter no-op**

In `crates/spur-core/src/lineage/adapter.rs`, add a no-op match arm for `DispatchOverlayApplied` (or wire to a new lineage column if the lineage schema supports it).

- [ ] **Step 15.5: Write a smoke test**

```rust
#[tokio::test]
async fn dispatch_emits_overlay_applied_event() {
    let harness = OrchestratorTestHarness::new().await;
    let request = DelegationRequest {
        // ... base = WithOverlay { ... } ...
    };
    let _ = harness.dispatch(request).await;

    let events = harness.collect_events().await;
    assert!(events.iter().any(|e| matches!(e.body, SpurEventBody::DispatchOverlayApplied { .. })));
}
```

- [ ] **Step 15.6: Run + commit**

```bash
cargo test --manifest-path crates/spur-core/Cargo.toml dispatch_emits_overlay_applied
cargo check --workspace
git add crates/spur-acp/src/event.rs crates/spur-core/src/orchestrator.rs crates/spur-core/src/lineage/adapter.rs
git commit -m "feat: DispatchOverlayApplied lineage event

Phase 1 of bd-1dwm. Records the BaseSpec, dispatched_base_oid, and
overlay task ids at the moment the worker worktree is finalized
(post-overlay, pre-agent-init). Without this, retries would silently
hide which overlay they ran against (related to the lineage adapter
blind spot at orchestrator.rs:6291-6297).

Refs: bd-1dwm"
```

---

## Task 16: Worker-output single-commit invariant + WorktreeAuthority adoption

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs` — assert at completion-audit time
- Modify: `crates/spur-core/src/worktree_authority.rs:112-124` — recognize new branch naming

### 16a — single-commit invariant

- [ ] **Step 16a.1: Write the failing test**

```rust
#[tokio::test]
async fn completion_audit_rejects_multi_commit_worker_output() {
    let harness = TestHarness::new().await;
    let plan_id = harness.submit_plan(/* 1-task plan */).await;

    // Worker produces 2 commits instead of 1.
    harness.dispatch_with_multi_commit_worker("T1", &[
        ("foo.rs", "first\n"),
        ("foo.rs", "second\n"), // second commit
    ]).await;

    let result = harness.complete("T1").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("multiple commits") || err.to_string().contains("invariant"));
}
```

- [ ] **Step 16a.2: Implement the assertion**

In `crates/spur-mcp/src/plan/audit_sentinel.rs` (or wherever completion audit runs), before persisting the audit:

```rust
let commit_count = git_count_commits(repo_root, &dispatched_base_oid, &worker_branch)?;
if commit_count > 1 {
    return Err(anyhow::anyhow!(
        "worker output invariant violated: branch {worker_branch} has {commit_count} commits in {dispatched_base_oid}..; expected 1. Squash or use --amend."
    ));
}

fn git_count_commits(repo: &Path, base: &str, head: &str) -> Result<usize> {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", &format!("{base}..{head}")])
        .current_dir(repo)
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse()?)
}
```

- [ ] **Step 16a.3: Run tests + commit**

```bash
cargo test --manifest-path crates/spur-mcp/Cargo.toml completion_audit_rejects_multi_commit
git add crates/spur-mcp/src/plan/audit_sentinel.rs
git commit -m "feat(spur-mcp): assert worker-output single-commit invariant

Phase 1 of bd-1dwm. merge_plan cherry-picks only the worker branch tip,
so multi-commit worker output silently drops earlier commits. Assert at
completion-audit time that dispatched_base_oid..worker_branch is exactly
one commit.

Refs: bd-1dwm"
```

### 16b — WorktreeAuthority recognizes new branches

- [ ] **Step 16b.1: Verify existing branch-naming convention**

Run: `grep -n 'spur/worker' crates/spur-worktree/src/manager.rs crates/spur-core/src/worktree_authority.rs`
Note current naming.

- [ ] **Step 16b.2: Add or adjust matcher**

If the new G-strict path uses the existing `spur/worker-{agent}-{session_id}` naming, no change needed. If it uses a new prefix (e.g., `spur/worker/v2/...`), add to `worktree_authority.rs`:

```rust
fn is_owned_branch(branch: &str) -> bool {
    branch.starts_with("refs/heads/spur/worker/v2/")
        || branch.starts_with("refs/heads/spur/worker-") // legacy + G-strict
}
```

Update the sweep loop to use this matcher instead of the v2-only check at line 113.

- [ ] **Step 16b.3: Run authority sweep test + commit**

```bash
cargo test --manifest-path crates/spur-core/Cargo.toml worktree_authority
git add crates/spur-core/src/worktree_authority.rs
git commit -m "fix(spur-core): WorktreeAuthority recognizes G-strict branches

Phase 1 of bd-1dwm. Without this, the GC sweep skips the new G-strict
worker branches, leaking refs over time.

Refs: bd-1dwm"
```

---

## Task 17: End-to-end synthetic bd-2dww reproducer

**Files:**
- Create: `crates/spur-mcp/tests/g_strict_e2e.rs`

Run a 3-task plan that mirrors the bd-2dww failure mode and assert zero LoC loss.

- [ ] **Step 17.1: Write the test**

```rust
//! End-to-end test: a 3-task plan where T2 modifies T1's new file and T3
//! imports both T1 and T2's symbols. Without G-strict, T2 and T3 lose
//! T1's content (the bd-2dww failure mode). With G-strict, all three
//! approve and merge cleanly.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_prevents_bd_2dww_class_loss() {
    let harness = TestHarness::new().await;
    let plan_id = harness.submit_plan(serde_json::json!({
        "tasks": [
            {
                "task_id": "T1",
                "agent": "mock",
                "task": "create foo.rs with `pub struct Foo { pub n: u32 }`",
                "depends_on": [],
            },
            {
                "task_id": "T2",
                "agent": "mock",
                "task": "modify foo.rs to add `impl Foo { pub fn new(n: u32) -> Self { Self { n } } }`",
                "depends_on": ["T1"],
            },
            {
                "task_id": "T3",
                "agent": "mock",
                "task": "create main.rs with `use foo::Foo; fn main() { let _ = Foo::new(42); }`",
                "depends_on": ["T1", "T2"],
            },
        ]
    })).await;

    // Drive each task and assert worker sees expected base.
    harness.dispatch_and_approve_with_mock("T1", |worktree| {
        std::fs::write(worktree.join("foo.rs"), "pub struct Foo { pub n: u32 }\n").unwrap();
    }).await;

    harness.dispatch_and_approve_with_mock("T2", |worktree| {
        // Worker MUST see foo.rs from T1.
        let existing = std::fs::read_to_string(worktree.join("foo.rs")).expect("T2 worker must see foo.rs from T1");
        assert!(existing.contains("pub struct Foo"));
        std::fs::write(worktree.join("foo.rs"), format!("{existing}\nimpl Foo {{ pub fn new(n: u32) -> Self {{ Self {{ n }} }} }}\n")).unwrap();
    }).await;

    harness.dispatch_and_approve_with_mock("T3", |worktree| {
        // Worker MUST see foo.rs (with both struct + impl) from T1+T2.
        let foo = std::fs::read_to_string(worktree.join("foo.rs")).expect("T3 worker must see foo.rs from T1+T2");
        assert!(foo.contains("pub struct Foo"));
        assert!(foo.contains("impl Foo"));
        std::fs::write(worktree.join("main.rs"), "use foo::Foo;\nfn main() { let _ = Foo::new(42); }\n").unwrap();
    }).await;

    // merge_plan must succeed with all 3 worker contributions.
    let merge_result = harness.merge_plan(&plan_id).await.unwrap();
    assert!(matches!(merge_result, PlanMergeState::Succeeded { .. }));

    // Verify final merge contains all 3 contributions intact.
    let merged = harness.get_merge_branch_contents(&plan_id).await;
    assert!(merged.get("foo.rs").unwrap().contains("pub struct Foo"));
    assert!(merged.get("foo.rs").unwrap().contains("impl Foo"));
    assert!(merged.get("main.rs").unwrap().contains("use foo::Foo"));
}
```

- [ ] **Step 17.2: Run the test**

Run: `cargo test --manifest-path crates/spur-mcp/Cargo.toml --test g_strict_e2e`
Expected: PASS.

- [ ] **Step 17.3: Run full workspace test + clippy**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
Expected: all green, no clippy warnings.

- [ ] **Step 17.4: Commit**

```bash
git add crates/spur-mcp/tests/g_strict_e2e.rs
git commit -m "test(spur-mcp): bd-2dww synthetic reproducer for G-strict

Phase 1 of bd-1dwm. Three-task plan where T2 modifies T1's new file and
T3 imports both. Without G-strict this is the canonical bd-2dww failure
mode (~700 LoC loss). With G-strict, T2's worker sees T1's foo.rs in
its base, T3's worker sees both, merge_plan succeeds with all
contributions intact.

Refs: bd-1dwm"
```

---

# Self-Review Checklist (run after Task 17)

Before declaring the plan complete:

- [ ] **Spec coverage:** Every section of `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md` is mapped to at least one task above. Specifically verify:
  - G-strict algorithm (steps 1-6 of Architecture) → Tasks 8 + 9 + 10
  - D detector → Tasks 1 + 2 + 3 + 4
  - BaseSpec API (additive Optional) → Task 5
  - dispatched_base_oid persistence → Tasks 6 + 7
  - preview_task_base → Task 13
  - Conflict-routing flow (5 steps) → Tasks 11 + 12
  - get_task_diff fix → Task 14
  - Worker-output single-commit invariant → Task 16a
  - WorktreeAuthority adoption → Task 16b
  - Lineage event → Task 15
  - End-to-end test → Task 17
- [ ] **Phase 0 standalone:** Tasks 1-4 form a complete shippable PR (signal enum + detector + handler wiring + UI badge + integration test).
- [ ] **Phase 1 ordering:** Tasks 5-17 follow strict dependency order — each later task references types/helpers from earlier tasks.
- [ ] **No placeholders:** No `TBD`, `TODO`, `implement later` outside genuine `todo!()` markers used as test scaffolding (which are filled in within the same task).
- [ ] **Type consistency:** `BaseSpec` / `OverlayCommit` / `dispatched_base_oid` / `BlockedOnSetupConflict` / `WorktreeError::OverlayConflict` / `AttemptSetupError::OverlayConflict` / `signal:integration-conflict` / `signal:potential-clobber` are spelled identically across all tasks.
- [ ] **`cargo clippy --workspace -- -D warnings`** passes after Task 17.
- [ ] **`cargo test --workspace`** passes after Task 17.

---

# Phase 2 — Out of Scope

The following are deferred to a separate spec + plan:

- `WithOverlay` for ad-hoc (non-plan) brain-initiated delegations
- `create_merge_task` MCP primitive for in-plan DAG-level conflict resolution
- Flip `base` from Optional to Required on `delegate_to_worker` after migration window
- Brain-side override of computed overlay for exceptional plan tasks

These are tracked under bd-1dwm Phase 2 follow-up (to be opened as a separate beads issue once Phase 1 ships).

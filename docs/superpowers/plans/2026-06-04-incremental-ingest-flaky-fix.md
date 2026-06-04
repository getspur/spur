# Fix flaky `tests/incremental_ingest.rs` Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Goal:** Make `crates/spur-graph/tests/incremental_ingest.rs` reliably pass. It is flaky — sometimes green, sometimes all 3 tests fail.

**Diagnosis (already done — start here, verify, then fix):**

1. **The cascade (definite).** `test_env_lock()` (line ~171) does `LOCK.get_or_init(...).lock().unwrap()`. The tests hold this guard for their whole body. When ONE test panics on an assertion, the `MutexGuard` drop poisons the static `Mutex`, so the next test's `.lock().unwrap()` returns `Err(PoisonError)` and panics. That is why a single real failure shows up as **all 3 failing** (1 real + 2 cascade `PoisonError`s). FIX (required): make the lock poison-resilient:
   ```rust
   fn test_env_lock() -> MutexGuard<'static, ()> {
       static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
       LOCK.get_or_init(|| Mutex::new(()))
           .lock()
           .unwrap_or_else(|poisoned| poisoned.into_inner())
   }
   ```
   This makes failures independent — a flaky/failing test no longer fails its siblings.

2. **The underlying flake: `incremental_run_full_walk_uses_prior_pointer_to_ingest_only_new_commits`** sometimes asserts `left: 8, right: 3` — i.e. the incremental second pass `git show`s ALL 8 commits instead of only the 3 new ones, meaning it did a FULL re-walk instead of using the saved prior pointer (`.spur/commit-index.json`). Leading hypothesis: `GitWrapperGuard` mutates **process-global env** (`std::env::set_var("PATH"/"SPUR_GIT_SHOW_LOG"/"SPUR_REAL_GIT")`), which is a **data race in a multithreaded test binary** (UB since Rust 2024; the toolchain here is 1.94.1) — and/or the prior-pointer/incremental decision inside `run_full_walk_into` is non-deterministic. INVESTIGATE and fix the root cause.

---

### Task fix: stop the cascade + root-cause the incremental flake

**Task ID:** `task-fix`

**Files:**
- Modify: `crates/spur-graph/tests/incremental_ingest.rs` (the env-lock fix, and any test-isolation hardening)
- Modify (only if the root cause is there): `crates/spur-graph/src/git_walk/**` (the `run_full_walk_into` incremental/prior-pointer path) — IN scope only if the flake is a real product bug, not a test bug.

**Depends on:** none

**Acceptance Criteria:**
- [ ] `test_env_lock` recovers from poison (no cascade): a single failing test no longer fails its siblings.
- [ ] `incremental_run_full_walk_uses_prior_pointer_to_ingest_only_new_commits` passes **reliably** — demonstrated by running the binary in a loop (see Step 3), e.g. 20 consecutive green runs.
- [ ] The fix's nature is reported: test-isolation bug (env race / shared state) vs a real `run_full_walk_into` bug. If it's a product bug, the fix is in `git_walk` with a clear explanation.
- [ ] No other spur-graph test regresses; clippy `-D warnings` clean.

**Suggested Worker:** claude-code-acp (debugging / judgment; reproduce-bisect-fix).

**Scope Boundary:** IN: the test file + (if proven) the `git_walk` incremental path. OUT: unrelated crates, the resolver/extraction ontology code, the analyst POC. Do NOT mask the flake by deleting/ignoring the assertion — fix the cause.

**Implementation:**

- [ ] **Step 1: Reproduce.** Run the binary repeatedly to confirm flakiness and capture a failing run:
  ```bash
  for i in $(seq 1 20); do SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test incremental_ingest 2>&1 | grep -E "test result|FAILED|left:|right:"; done
  ```
  Note whether failures correlate with default parallelism vs `--test-threads=1`.

- [ ] **Step 2: Apply the cascade fix** (poison-resilient `test_env_lock`, code above). Re-run; confirm that when a failure occurs it is now a SINGLE test, not 3.

- [ ] **Step 3: Root-cause the `ingest_only_new_commits` flake.** Investigate the two leads:
  - **Env data race:** the `GitWrapperGuard` + `restore_var` use `std::env::set_var`/`remove_var`. Even with `test_env_lock` serializing the bodies, confirm nothing in `run_full_walk_into` (or a sibling thread) reads `PATH`/the SPUR_* vars concurrently. If the wrapper install/restore is the source, harden it (e.g. ensure the guard fully brackets every `git` invocation the walk makes; consider that the walk may shell out to `git` which reads the live `PATH`).
  - **Prior-pointer non-determinism:** in `src/git_walk/`, trace how `run_full_walk_into(repo, config, None, None)` decides incremental-vs-full from `.spur/commit-index.json`. Determine why it sometimes ignores the saved pointer and re-walks all commits. Fix the actual non-determinism (e.g. an ordering/timestamp/ref-comparison bug), not the test.
  - Use `code_*` / `code_search` to navigate `git_walk` rather than text-walking.

- [ ] **Step 4: Prove it.** Run Step 1's loop again → 20/20 green (excluding nothing — this binary must be fully green now). Then the broad gate:
  ```bash
  SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test incremental_ingest   # must be green
  SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
  ```

- [ ] **Step 5: Commit.**
  ```bash
  git add crates/spur-graph/tests/incremental_ingest.rs  # + src/git_walk/... if a product bug
  git commit -m "fix(spur-graph): de-flake incremental_ingest (poison-resilient env lock + <root cause>)"
  ```
  Report the root cause and whether the fix was test-side or product-side.

**Scope Drift Checkpoint:** if the root cause turns out to be a deep product bug requiring a non-trivial `git_walk` change (>~1 file or a behavior change to the incremental algorithm), STOP after landing the cascade fix (Step 2) and emit `risk` with your findings, so the brain can decide whether to scope a separate product fix.

## Self-Review
- **Coverage:** fixes the "all 3 fail" cascade (poison) definitively, and drives the underlying flake to root cause.
- **No placeholders:** the poison fix is exact; the investigation has concrete leads (env race, prior-pointer) and a reproduction loop.
- **Risk:** bounded — the cascade fix is one line; the deeper investigation has a `risk` off-ramp if it becomes a product-behavior change.

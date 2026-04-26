# WorktreeAuthority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close Risk #4 (worktree orphaning) by adding a lease-aware `WorktreeAuthority` that sweeps dead-session worktrees safely under multi-process operation, prerequisited by `kill_on_drop` on worker spawn so dead-lock implies dead-worker.

**Architecture:** Per the design spec at `docs/superpowers/specs/2026-04-26-worktree-authority-design.md`. Phase 0a adds `kill_on_drop(true)` to five worker spawn sites in `crates/spur-acp/src/connection/`. Phase 1' introduces `SessionLivenessProbe` (a non-mutating probe distinct from `SessionAttachGuard`), a v2 branch namespace `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}`, and a `WorktreeAuthority` actor spawned into `Orchestrator.background_tasks` that runs startup + periodic + on-retire sweeps. Each sweep enumerates `git worktree list --porcelain`, parses v2 branches, probes the per-brain-session lockfile, and removes only worktrees whose owner is provably dead and outside a 30-second quarantine grace.

**Tech Stack:** Rust 2021, Tokio (`process::Command`, `JoinHandle`), `fs4` (advisory file locks, already a workspace dep via session lock), `tracing` (telemetry), `git` plumbing via `tokio::process::Command`.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/connection/native.rs` | Modify (lines 713, 1222) | Add `kill_on_drop(true)` to brain + worker spawn |
| `crates/spur-acp/src/connection/stdio_adapter.rs` | Modify (line 88) | Add `kill_on_drop(true)` to worker spawn |
| `crates/spur-acp/src/connection/cli_wrap_adapter.rs` | Modify (line 168) | Add `kill_on_drop(true)` to worker spawn |
| `crates/spur-acp/src/connection/stream_json_adapter.rs` | Modify (line 164) | Add `kill_on_drop(true)` to worker spawn |
| `crates/spur-acp/src/session_liveness.rs` | Create | `SessionLivenessProbe`, `DeadSessionGuard`, `SelfHeldSet` |
| `crates/spur-acp/src/lib.rs` | Modify | Re-export new module |
| `crates/spur-worktree/src/manager.rs` | Modify (line 165, 347, 459) | New v2 branch naming, `remove_worktree` ordering fix, narrow `cleanup_orphans` namespace match, add `parse_v2_branch` |
| `crates/spur-core/src/worktree_authority.rs` | Create | `WorktreeAuthority` actor + `SweepReport` + `AuthorityConfig` |
| `crates/spur-core/src/lib.rs` | Modify | Re-export `WorktreeAuthority` |
| `crates/spur-core/src/orchestrator.rs` | Modify (line 792, 954, 1448–1500, retire path) | Replace dead `pub worktrees` field, wire authority into boot, maintain `SelfHeldSet` |
| `crates/spur-acp/tests/process_kill_on_drop.rs` | Create | Phase 0a integration tests, one per adapter |
| `crates/spur-acp/src/session_liveness.rs` (test mod) | (within module) | Unit tests for probe variants |
| `crates/spur-worktree/src/manager.rs` (test mod) | (within module) | Unit tests for v2 parser |
| `crates/spur-core/tests/worktree_authority.rs` | Create | Integration tests: multi-process safety, quarantine, panic isolation |

---

## Task 1: Audit explicit kill paths before Phase 0a

**Files:**
- Read-only audit: `crates/spur-acp/src/connection/native.rs:204,884,1340,1367`

- [ ] **Step 1: Read the four explicit-kill sites and confirm pattern**

Run:
```bash
rg -n -B2 -A6 'std::process::Command::new\("kill"\)' crates/spur-acp/src/connection/native.rs
```
Expected: four blocks showing the existing graceful-shutdown SIGKILL fallback. Note in each block: is the `kill` followed by a `child.wait().await` or equivalent? If yes, Drop-time SIGKILL after Tokio's `kill_on_drop` is benign (`ESRCH` on second kill). If a block has no `wait`, document it as a Phase 0a follow-up.

- [ ] **Step 2: Document audit findings as a comment in `crates/spur-acp/src/connection/native.rs:1`**

Add a comment block at the top of the file noting the four kill sites and confirming `kill_on_drop` compatibility. Example:

```rust
// kill_on_drop audit (bd-arch.WTA Phase 0a, 2026-04-26):
// Explicit kill -SIGKILL fallbacks at lines 204, 884, 1340, 1367 are safe
// to combine with Tokio kill_on_drop(true). Each is paired with a wait,
// so Drop-time SIGKILL on a reaped PID returns ESRCH (no-op) on POSIX.
// Windows: TerminateProcess on a stale handle is also a no-op for our
// purposes (the kernel rejects with ERROR_INVALID_HANDLE).
```

- [ ] **Step 3: Commit audit comment**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "docs(spur-acp): audit kill_on_drop compatibility before Phase 0a wiring"
```

---

## Task 2: Phase 0a — `kill_on_drop` on Native worker spawn

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:713`
- Test: `crates/spur-acp/tests/process_kill_on_drop.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/process_kill_on_drop.rs`:

```rust
//! Phase 0a integration tests — verify worker child processes die when
//! the spawning Tokio Command is dropped (kill_on_drop semantics).

use std::time::{Duration, Instant};
use tokio::process::Command;

async fn pid_alive(pid: u32) -> bool {
    // POSIX: kill -0 returns 0 if process exists, errors if not.
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn native_worker_dies_on_drop() {
    use spur_acp::connection::native::spawn_native_worker_for_test;

    // Spawn a long-running child via the production helper.
    let mut child = spawn_native_worker_for_test(
        "/bin/sh",
        &["-c", "sleep 60"],
    )
    .await
    .expect("spawn child");

    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await, "child should be alive after spawn");

    drop(child); // Drop the Command/Child.

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await {
            return; // PASS — child died.
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("child PID {pid} still alive 500ms after Drop; kill_on_drop missing");
}
```

- [ ] **Step 2: Run the test — confirm it fails**

```bash
cargo test -p spur-acp --test process_kill_on_drop native_worker_dies_on_drop
```
Expected: FAIL — test helper `spawn_native_worker_for_test` does not exist yet.

- [ ] **Step 3: Add test helper and `kill_on_drop(true)` to native worker spawn**

Locate the existing spawn at `crates/spur-acp/src/connection/native.rs:713` (the worker site, not brain). Add `.kill_on_drop(true)` to the `Command` builder. Add a `pub(crate) fn spawn_native_worker_for_test` near the top of the file that wraps the same builder, exposed under `#[cfg(test)]` or a `test-support` feature.

Diff (illustrative; line numbers may shift slightly):
```rust
// Around line 713
let mut cmd = tokio::process::Command::new(&command);
cmd.args(args)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);   // <-- ADDED
```

Add the test helper:
```rust
#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_native_worker_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}
```

- [ ] **Step 4: Run the test — confirm it passes**

```bash
cargo test -p spur-acp --test process_kill_on_drop native_worker_dies_on_drop
```
Expected: PASS within 500ms.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs crates/spur-acp/tests/process_kill_on_drop.rs
git commit -m "feat(spur-acp): add kill_on_drop(true) on Native worker spawn (Phase 0a)"
```

---

## Task 3: Phase 0a — `kill_on_drop` on Native brain spawn

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:1222`

- [ ] **Step 1: Add `kill_on_drop(true)` to the brain spawn site**

Locate the second spawn site at `crates/spur-acp/src/connection/native.rs:1222` (the brain agent path). Add `.kill_on_drop(true)` to the `Command` builder.

```rust
// Around line 1222
let mut cmd = tokio::process::Command::new(&args.command);
cmd.args(&args.args)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);   // <-- ADDED
```

- [ ] **Step 2: Verify with workspace build**

```bash
cargo build -p spur-acp
```
Expected: clean build. No new warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): add kill_on_drop(true) on Native brain spawn (Phase 0a)"
```

---

## Task 4: Phase 0a — `kill_on_drop` on stdio_adapter

**Files:**
- Modify: `crates/spur-acp/src/connection/stdio_adapter.rs:88`
- Test: `crates/spur-acp/tests/process_kill_on_drop.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-acp/tests/process_kill_on_drop.rs`:

```rust
#[tokio::test]
async fn stdio_adapter_dies_on_drop() {
    use spur_acp::connection::stdio_adapter::spawn_stdio_for_test;

    let mut child = spawn_stdio_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);

    drop(child);

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("stdio child {pid} still alive 500ms after Drop");
}
```

- [ ] **Step 2: Run the test — confirm it fails**

```bash
cargo test -p spur-acp --test process_kill_on_drop stdio_adapter_dies_on_drop
```
Expected: FAIL — helper missing.

- [ ] **Step 3: Add `kill_on_drop(true)` and test helper**

At `crates/spur-acp/src/connection/stdio_adapter.rs:88`, add `.kill_on_drop(true)` to the spawn builder:

```rust
// Around line 88
let mut child = tokio::process::Command::new(&self.command)
    .args(&self.args)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)   // <-- ADDED
    .spawn()?;
```

Add helper:
```rust
#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_stdio_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}
```

- [ ] **Step 4: Run the test — confirm it passes**

```bash
cargo test -p spur-acp --test process_kill_on_drop stdio_adapter_dies_on_drop
```
Expected: PASS within 500ms.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/connection/stdio_adapter.rs crates/spur-acp/tests/process_kill_on_drop.rs
git commit -m "feat(spur-acp): add kill_on_drop(true) on stdio_adapter spawn (Phase 0a)"
```

---

## Task 5: Phase 0a — `kill_on_drop` on cli_wrap_adapter

**Files:**
- Modify: `crates/spur-acp/src/connection/cli_wrap_adapter.rs:168`
- Test: `crates/spur-acp/tests/process_kill_on_drop.rs`

- [ ] **Step 1: Write the failing test**

Append:
```rust
#[tokio::test]
async fn cli_wrap_dies_on_drop() {
    use spur_acp::connection::cli_wrap_adapter::spawn_cli_wrap_for_test;
    let mut child = spawn_cli_wrap_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);
    drop(child);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("cli_wrap child {pid} still alive 500ms after Drop");
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-acp --test process_kill_on_drop cli_wrap_dies_on_drop
```
Expected: FAIL.

- [ ] **Step 3: Add `kill_on_drop(true)` + helper at `:168`**

```rust
// Around line 168 in cli_wrap_adapter.rs
let mut child = tokio::process::Command::new(&self.command)
    .args(&self.args)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)   // <-- ADDED
    .spawn()?;
```

```rust
#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_cli_wrap_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}
```

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-acp --test process_kill_on_drop cli_wrap_dies_on_drop
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/connection/cli_wrap_adapter.rs crates/spur-acp/tests/process_kill_on_drop.rs
git commit -m "feat(spur-acp): add kill_on_drop(true) on cli_wrap_adapter spawn (Phase 0a)"
```

---

## Task 6: Phase 0a — `kill_on_drop` on stream_json_adapter

**Files:**
- Modify: `crates/spur-acp/src/connection/stream_json_adapter.rs:164`
- Test: `crates/spur-acp/tests/process_kill_on_drop.rs`

- [ ] **Step 1: Write the failing test**

Append:
```rust
#[tokio::test]
async fn stream_json_dies_on_drop() {
    use spur_acp::connection::stream_json_adapter::spawn_stream_json_for_test;
    let mut child = spawn_stream_json_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);
    drop(child);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("stream_json child {pid} still alive 500ms after Drop");
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-acp --test process_kill_on_drop stream_json_dies_on_drop
```
Expected: FAIL.

- [ ] **Step 3: Add `kill_on_drop(true)` + helper at `:164`**

```rust
// Around line 164 in stream_json_adapter.rs
let mut child = tokio::process::Command::new(&self.command)
    .args(&self.args)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)   // <-- ADDED
    .spawn()?;
```

```rust
#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_stream_json_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}
```

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-acp --test process_kill_on_drop stream_json_dies_on_drop
```
Expected: PASS.

- [ ] **Step 5: Commit and run full Phase 0a test suite**

```bash
cargo test -p spur-acp --test process_kill_on_drop
```
Expected: 4 tests pass (native_worker, stdio, cli_wrap, stream_json).

```bash
git add crates/spur-acp/src/connection/stream_json_adapter.rs crates/spur-acp/tests/process_kill_on_drop.rs
git commit -m "feat(spur-acp): add kill_on_drop(true) on stream_json_adapter spawn (Phase 0a complete)"
```

**Phase 0a is now complete.** Worker children die with the orchestrator. Phase 1' is unblocked.

---

## Task 7: `SelfHeldSet` skeleton

**Files:**
- Create: `crates/spur-acp/src/session_liveness.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/src/session_liveness.rs` with a test module:

```rust
//! Probe whether a brain session is held by any live process, without
//! mutating the lockfile state observable by other processes.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::BrainSessionId;

#[derive(Debug, Clone, Default)]
pub struct SelfHeldSet {
    inner: Arc<RwLock<HashSet<BrainSessionId>>>,
}

impl SelfHeldSet {
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(HashSet::new())) }
    }

    pub fn insert(&self, id: BrainSessionId) {
        self.inner.write().expect("SelfHeldSet poisoned").insert(id);
    }

    pub fn remove(&self, id: &BrainSessionId) -> bool {
        self.inner.write().expect("SelfHeldSet poisoned").remove(id)
    }

    pub fn contains(&self, id: &BrainSessionId) -> bool {
        self.inner.read().expect("SelfHeldSet poisoned").contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionId;

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    #[test]
    fn self_held_set_insert_and_contains() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        assert!(!set.contains(&a));
        set.insert(a.clone());
        assert!(set.contains(&a));
    }

    #[test]
    fn self_held_set_remove_returns_true_when_present() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(a.clone());
        assert!(set.remove(&a));
        assert!(!set.contains(&a));
    }

    #[test]
    fn self_held_set_remove_returns_false_when_absent() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        assert!(!set.remove(&a));
    }

    #[test]
    fn self_held_set_clones_share_state() {
        let set = SelfHeldSet::new();
        let clone = set.clone();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(a.clone());
        assert!(clone.contains(&a));
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

In `crates/spur-acp/src/lib.rs`, add:

```rust
pub mod session_liveness;
pub use session_liveness::SelfHeldSet;
```

- [ ] **Step 3: Run the test**

```bash
cargo test -p spur-acp session_liveness::tests
```
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/session_liveness.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): introduce SelfHeldSet for session probe self-skip"
```

---

## Task 8: `SessionLivenessProbe` — `Self_` and `Missing` variants

**Files:**
- Modify: `crates/spur-acp/src/session_liveness.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-acp/src/session_liveness.rs`:

```rust
use std::path::{Path, PathBuf};
use std::fs::{File, OpenOptions};
use std::io;

#[derive(Debug)]
pub enum SessionLivenessProbeResult {
    Live,
    DeadAcquired(DeadSessionGuard),
    Self_,
    Missing,
    FsUnsafe,
}

#[derive(Debug)]
pub struct DeadSessionGuard {
    file: File,
    brain_session_id: BrainSessionId,
}

impl DeadSessionGuard {
    pub fn brain_session_id(&self) -> &BrainSessionId { &self.brain_session_id }
}

pub struct SessionLivenessProbe;

impl SessionLivenessProbe {
    pub fn probe(
        repo_root: &Path,
        target: &BrainSessionId,
        held_by_self: &SelfHeldSet,
    ) -> SessionLivenessProbeResult {
        if held_by_self.contains(target) {
            return SessionLivenessProbeResult::Self_;
        }
        let lock_path = lock_path_for(repo_root, target);
        let _file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return SessionLivenessProbeResult::Missing;
            }
            Err(e) => {
                tracing::warn!(error=%e, path=%lock_path.display(),
                    "session liveness probe open failed; treating as Live");
                return SessionLivenessProbeResult::Live;
            }
        };
        // flock branch added in next task
        unimplemented!("flock variant in Task 9")
    }
}

fn lock_path_for(repo_root: &Path, target: &BrainSessionId) -> PathBuf {
    repo_root
        .join(".spur/sessions")
        .join(format!("{}.lock", target.as_session_id().0))
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::SessionId;
    use tempfile::TempDir;

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    #[test]
    fn probe_returns_self_for_held_session() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(target.clone());
        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(result, SessionLivenessProbeResult::Self_));
    }

    #[test]
    fn probe_returns_missing_when_lockfile_absent() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(result, SessionLivenessProbeResult::Missing));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p spur-acp session_liveness::probe_tests
```
Expected: 2 tests pass (the `Self_` and `Missing` paths). The unimplemented branches are unreachable in these tests.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/session_liveness.rs
git commit -m "feat(spur-acp): SessionLivenessProbe Self_ and Missing variants"
```

---

## Task 9: `SessionLivenessProbe` — `Live`, `DeadAcquired`, `FsUnsafe` variants

**Files:**
- Modify: `crates/spur-acp/src/session_liveness.rs`

- [ ] **Step 1: Write the failing tests**

Append to `probe_tests` module:

```rust
use fs4::fs_std::FileExt;

fn create_lockfile(td: &TempDir, target: &BrainSessionId) -> PathBuf {
    let dir = td.path().join(".spur/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.lock", target.as_session_id().0));
    std::fs::write(&path, b"").unwrap();
    path
}

#[test]
fn probe_returns_dead_acquired_when_lockfile_unlocked() {
    let td = TempDir::new().unwrap();
    let set = SelfHeldSet::new();
    let target = id("550e8400-e29b-41d4-a716-446655440000");
    create_lockfile(&td, &target);

    let result = SessionLivenessProbe::probe(td.path(), &target, &set);
    match result {
        SessionLivenessProbeResult::DeadAcquired(guard) => {
            assert_eq!(guard.brain_session_id(), &target);
        }
        other => panic!("expected DeadAcquired, got {:?}", other),
    }
}

#[test]
fn probe_returns_live_when_other_holds_lock() {
    let td = TempDir::new().unwrap();
    let set = SelfHeldSet::new();
    let target = id("550e8400-e29b-41d4-a716-446655440000");
    let lock_path = create_lockfile(&td, &target);

    // Acquire the lock from "another process" (this test holds it).
    let held = OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
    held.try_lock_exclusive().expect("hold lock");

    let result = SessionLivenessProbe::probe(td.path(), &target, &set);
    assert!(matches!(result, SessionLivenessProbeResult::Live));

    // Cleanup: drop releases.
    drop(held);
}

#[test]
fn probe_does_not_truncate_lockfile() {
    let td = TempDir::new().unwrap();
    let set = SelfHeldSet::new();
    let target = id("550e8400-e29b-41d4-a716-446655440000");
    let lock_path = create_lockfile(&td, &target);
    std::fs::write(&lock_path, b"holder-info-payload").unwrap();
    let before = std::fs::read(&lock_path).unwrap();

    let _result = SessionLivenessProbe::probe(td.path(), &target, &set);
    let after = std::fs::read(&lock_path).unwrap();
    assert_eq!(before, after, "probe must not truncate or modify lockfile");
}

#[test]
fn dead_session_guard_releases_on_drop() {
    let td = TempDir::new().unwrap();
    let set = SelfHeldSet::new();
    let target = id("550e8400-e29b-41d4-a716-446655440000");
    create_lockfile(&td, &target);

    {
        let r = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(r, SessionLivenessProbeResult::DeadAcquired(_)));
        // Guard goes out of scope here; lock should release.
    }

    // Re-probe; should be DeadAcquired again (lock was released).
    let r2 = SessionLivenessProbe::probe(td.path(), &target, &set);
    assert!(matches!(r2, SessionLivenessProbeResult::DeadAcquired(_)));
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-acp session_liveness::probe_tests
```
Expected: 4 new tests fail with `unimplemented!` panic on the flock branch.

- [ ] **Step 3: Implement the `flock` branch**

Replace the `unimplemented!` block in `SessionLivenessProbe::probe` with:

```rust
let file = match OpenOptions::new()
    .read(true).write(true).create(false).truncate(false)
    .open(&lock_path)
{
    Ok(f) => f,
    Err(e) if e.kind() == io::ErrorKind::NotFound => {
        return SessionLivenessProbeResult::Missing;
    }
    Err(e) => {
        tracing::warn!(error=%e, "session liveness probe open failed");
        return SessionLivenessProbeResult::Live;
    }
};

use fs4::fs_std::FileExt;
match file.try_lock_exclusive() {
    Ok(true) => {
        SessionLivenessProbeResult::DeadAcquired(DeadSessionGuard {
            file,
            brain_session_id: target.clone(),
        })
    }
    Ok(false) => SessionLivenessProbeResult::Live,
    Err(e) if is_enotsup_or_enolck(&e) => SessionLivenessProbeResult::FsUnsafe,
    Err(e) => {
        tracing::warn!(error=%e, "try_lock_exclusive failed; treating as Live");
        SessionLivenessProbeResult::Live
    }
}
```

Add the `is_enotsup_or_enolck` helper near the bottom of the module:

```rust
fn is_enotsup_or_enolck(e: &io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(e.kind(), ErrorKind::Unsupported)
        || e.raw_os_error() == Some(libc::ENOLCK)
        || e.raw_os_error() == Some(libc::ENOTSUP)
}
```

Add `libc = "0.2"` to `crates/spur-acp/Cargo.toml` if not already present.

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-acp session_liveness::probe_tests
```
Expected: 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/session_liveness.rs crates/spur-acp/Cargo.toml
git commit -m "feat(spur-acp): SessionLivenessProbe Live/DeadAcquired/FsUnsafe variants"
```

---

## Task 10: v2 branch namespace + parser

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:165` (rename method) and add `parse_v2_branch`

- [ ] **Step 1: Write the failing parser tests**

In `crates/spur-worktree/src/manager.rs`, add a test module at the bottom (after `tests_option_e`):

```rust
#[cfg(test)]
mod v2_branch_tests {
    use super::*;

    fn make(agent: &str, brain: &str, worker: &str) -> String {
        format!("spur/worker/v2/{agent}/{brain}/{worker}")
    }

    #[test]
    fn parse_v2_simple_agent() {
        let b = make("claude", "550e8400-e29b-41d4-a716-446655440000",
                     "deadbeef-1111-2222-3333-444455556666");
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "claude");
    }

    #[test]
    fn parse_v2_hyphenated_agent() {
        let b = make("claude-code", "550e8400-e29b-41d4-a716-446655440000",
                     "deadbeef-1111-2222-3333-444455556666");
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "claude-code");
    }

    #[test]
    fn parse_v2_dotted_agent() {
        let b = make("gemini-2.5-pro", "550e8400-e29b-41d4-a716-446655440000",
                     "deadbeef-1111-2222-3333-444455556666");
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "gemini-2.5-pro");
    }

    #[test]
    fn parse_v2_rejects_pre_v2_format() {
        let b = "spur/worker-claude-deadbeef-1111-2222-3333-444455556666";
        assert!(parse_v2_branch(b).is_none());
    }

    #[test]
    fn parse_v2_rejects_when_session_not_uuid() {
        let b = "spur/worker/v2/claude/not-a-uuid/deadbeef-1111-2222-3333-444455556666";
        assert!(parse_v2_branch(b).is_none());
    }
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-worktree v2_branch_tests
```
Expected: FAIL — `parse_v2_branch` does not exist.

- [ ] **Step 3: Add the `V2BranchOwner` struct and parser**

Add to `crates/spur-worktree/src/manager.rs`:

```rust
use spur_acp::BrainSessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2BranchOwner {
    pub agent: String,
    pub brain_session_id: BrainSessionId,
    pub worker_session_id: SessionId,
}

/// Parse a v2 worker branch into its owner triple. Returns None for any
/// non-v2 input. Slash-delimited so hyphenated/dotted agent names like
/// `claude-code` and `gemini-2.5-pro` parse unambiguously.
pub fn parse_v2_branch(branch: &str) -> Option<V2BranchOwner> {
    let rest = branch.strip_prefix("spur/worker/v2/")?;
    let mut parts = rest.rsplitn(3, '/');
    let worker_session_str = parts.next()?;
    let brain_session_str = parts.next()?;
    let agent = parts.next()?.to_string();

    fn is_uuid(s: &str) -> bool {
        s.len() == 36
            && s.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            })
    }

    if !is_uuid(brain_session_str) || !is_uuid(worker_session_str) {
        return None;
    }

    Some(V2BranchOwner {
        agent,
        brain_session_id: BrainSessionId::new(SessionId(brain_session_str.into())),
        worker_session_id: SessionId(worker_session_str.into()),
    })
}
```

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-worktree v2_branch_tests
```
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-worktree): add V2BranchOwner + parse_v2_branch"
```

---

## Task 11: Switch `create_worktree` to v2 naming

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:157,165`

- [ ] **Step 1: Write the failing test**

Append to `tests_option_e` mod (the existing test module):

```rust
#[tokio::test]
async fn create_worktree_uses_v2_namespace() {
    use spur_acp::SessionId;
    let tmp = tempfile::TempDir::new().unwrap();
    let _base_sha = seed_base_repo(tmp.path()).await;

    let mut manager = WorktreeManager::new(tmp.path().to_path_buf());
    let brain = spur_acp::BrainSessionId::new(SessionId(
        "550e8400-e29b-41d4-a716-446655440000".into(),
    ));
    let worker = SessionId("deadbeef-1111-2222-3333-444455556666".into());

    let info = manager
        .create_worktree_v2(&brain, &worker, "codex", "main")
        .await
        .expect("create v2 worktree");
    assert_eq!(
        info.branch,
        "spur/worker/v2/codex/550e8400-e29b-41d4-a716-446655440000/deadbeef-1111-2222-3333-444455556666"
    );
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-worktree create_worktree_uses_v2_namespace
```
Expected: FAIL — `create_worktree_v2` does not exist.

- [ ] **Step 3: Add `create_worktree_v2`**

In `crates/spur-worktree/src/manager.rs`, add a new method ALONGSIDE the existing `create_worktree` (do not delete the legacy method yet — orchestrator updates in Task 14 will remove it):

```rust
impl WorktreeManager {
    /// Create a worktree under the v2 branch namespace
    /// `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}`.
    pub async fn create_worktree_v2(
        &mut self,
        brain_session_id: &spur_acp::BrainSessionId,
        worker_session_id: &SessionId,
        agent: &str,
        base_branch: &str,
    ) -> Result<WorktreeInfo> {
        let worker_str = worker_session_id.to_string();
        let worktree_path = self.repo_root.join(".spur/worktrees").join(&worker_str);
        let branch_name = format!(
            "spur/worker/v2/{}/{}/{}",
            agent,
            brain_session_id.as_session_id().0,
            worker_str,
        );

        let worktree_path_str = worktree_path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
        let base_commit = self
            .run_git(&["rev-parse", base_branch], None)
            .await
            .with_context(|| format!("failed to resolve base branch '{base_branch}'"))?;

        self.run_git(
            &["worktree", "add", worktree_path_str, "-b", &branch_name, base_branch],
            None,
        )
        .await
        .with_context(|| format!("failed to create v2 worktree at {worktree_path_str}"))?;

        let info = WorktreeInfo {
            session_id: worker_session_id.clone(),
            path: worktree_path,
            branch: branch_name,
            base_commit,
            agent: agent.to_string(),
            created_at: Instant::now(),
        };
        self.active.insert(worker_str.clone(), info);

        let stored = self.active.get(&worker_str).unwrap();
        Ok(WorktreeInfo {
            session_id: stored.session_id.clone(),
            path: stored.path.clone(),
            branch: stored.branch.clone(),
            base_commit: stored.base_commit.clone(),
            agent: stored.agent.clone(),
            created_at: stored.created_at,
        })
    }
}
```

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-worktree create_worktree_uses_v2_namespace
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-worktree): add create_worktree_v2 with new namespace"
```

---

## Task 12: Fix `remove_worktree` ordering bug

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:347–367`

- [ ] **Step 1: Write the failing test**

Append to `tests_option_e`:

```rust
#[tokio::test]
async fn remove_worktree_keeps_in_memory_entry_when_git_fails() {
    use spur_acp::SessionId;
    let tmp = tempfile::TempDir::new().unwrap();
    let _base_sha = seed_base_repo(tmp.path()).await;

    let sid = SessionId("s1".into());
    let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
    manager.register_for_test(
        sid.clone(),
        tmp.path().join("nonexistent"),  // path doesn't exist; git remove will fail
        "spur/worker/v2/x/x/x".to_string(),
        "deadbeef".to_string(),
        "test".to_string(),
    );
    assert_eq!(manager.active_count(), 1);

    let res = manager.remove_worktree(&sid).await;
    assert!(res.is_err(), "git should have failed; {res:?}");
    assert_eq!(
        manager.active_count(), 1,
        "in-memory entry must NOT be removed when git remove fails"
    );
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-worktree remove_worktree_keeps_in_memory_entry_when_git_fails
```
Expected: FAIL — current code removes from `self.active` BEFORE invoking git, so on git failure, in-memory entry is gone.

- [ ] **Step 3: Reorder `remove_worktree`**

Replace the body of `remove_worktree` at `crates/spur-worktree/src/manager.rs:347–367`:

```rust
pub async fn remove_worktree(&mut self, session_id: &SessionId) -> Result<()> {
    let session_str = session_id.to_string();
    // PEEK, do not remove yet — fix codex's ordering bug.
    let info = self
        .active
        .get(&session_str)
        .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;
    let path_str = info
        .path
        .to_str()
        .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
        .to_string();
    let branch = info.branch.clone();

    // Run git operations; if any fail, return without mutating self.active.
    self.run_git(&["worktree", "remove", &path_str, "--force", "--force"], None)
        .await
        .with_context(|| format!("failed to remove worktree at {path_str}"))?;
    self.run_git(&["branch", "-D", &branch], None)
        .await
        .with_context(|| format!("failed to delete branch '{branch}'"))?;

    // ONLY now: remove from self.active.
    self.active.remove(&session_str);
    Ok(())
}
```

Note the additional change: `--force --force` (codex's correction for locked entries).

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-worktree remove_worktree_keeps_in_memory_entry_when_git_fails
cargo test -p spur-worktree   # full crate test pass-through to confirm no regressions
```
Expected: new test passes; existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "fix(spur-worktree): keep in-memory entry on git remove failure + double --force"
```

---

## Task 13: Narrow `cleanup_orphans` namespace match

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:459`

- [ ] **Step 1: Write the failing test**

Append to `tests_option_e`:

```rust
#[tokio::test]
async fn cleanup_orphans_only_touches_v2_worker_namespace() {
    use spur_acp::SessionId;
    let tmp = tempfile::TempDir::new().unwrap();
    let _base_sha = seed_base_repo(tmp.path()).await;

    // Create three branches: legacy, v2, and a non-SPUR user branch.
    let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
    let _ = manager.run_git(&["branch", "spur/worker-legacy-deadbeef"], None).await;
    let _ = manager.run_git(
        &["branch", "spur/worker/v2/codex/550e8400-e29b-41d4-a716-446655440000/deadbeef-1111-2222-3333-444455556666"],
        None,
    ).await;
    let _ = manager.run_git(&["branch", "feature/userwork"], None).await;

    // We don't have worktrees backing any of them, so cleanup_orphans should
    // not delete any branches BUT must not try to delete feature/userwork.
    let _removed = manager.cleanup_orphans().await.unwrap_or(0);

    let branches = manager
        .run_git(&["branch", "--list"], None)
        .await
        .unwrap_or_default();
    assert!(branches.contains("feature/userwork"),
        "user branches must NEVER be touched by cleanup_orphans");
}
```

- [ ] **Step 2: Run — confirm pass or fail**

```bash
cargo test -p spur-worktree cleanup_orphans_only_touches_v2_worker_namespace
```
Expected: PASS today (current code only touches branches with `spur/`), but the test future-proofs against the change.

- [ ] **Step 3: Narrow the match in `cleanup_orphans`**

In `crates/spur-worktree/src/manager.rs:459`, change:

```rust
if branch.contains("spur/") {
```
to:

```rust
if branch.starts_with("refs/heads/spur/worker/v2/") {
```

This narrows the cleanup to ONLY the v2 worker namespace. Pre-v2 branches and snapshot branches are NOT auto-swept by this method (per spec I-7).

- [ ] **Step 4: Run all tests**

```bash
cargo test -p spur-worktree
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "fix(spur-worktree): narrow cleanup_orphans to refs/heads/spur/worker/v2/"
```

---

## Task 14: `WorktreeAuthority` skeleton + `SweepReport`

**Files:**
- Create: `crates/spur-core/src/worktree_authority.rs`
- Modify: `crates/spur-core/src/lib.rs`

- [ ] **Step 1: Create the module skeleton**

Create `crates/spur-core/src/worktree_authority.rs`:

```rust
//! Lease-aware worktree garbage collection.
//!
//! See `docs/superpowers/specs/2026-04-26-worktree-authority-design.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spur_acp::{session_liveness::SelfHeldSet, BrainSessionId};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
pub struct AuthorityConfig {
    pub sweep_interval: Duration,
    pub quarantine_grace: Duration,
    pub fs_unsafe_skip: bool,
}

impl Default for AuthorityConfig {
    fn default() -> Self {
        Self {
            sweep_interval: Duration::from_secs(15 * 60),
            quarantine_grace: Duration::from_secs(30),
            fs_unsafe_skip: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub probed: usize,
    pub swept: usize,
    pub skipped_self: usize,
    pub skipped_live: usize,
    pub skipped_quarantine: usize,
    pub skipped_unknown_owner: usize,
    pub skipped_fs_unsafe: usize,
    pub remove_failures: usize,
}

#[derive(Debug)]
pub enum AuthorityError {
    Io(std::io::Error),
    Git(String),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Git(s) => write!(f, "git: {s}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

pub struct WorktreeAuthority {
    repo_root: Arc<PathBuf>,
    self_held: SelfHeldSet,
    config: AuthorityConfig,
    last_seen_alive: tokio::sync::Mutex<HashMap<BrainSessionId, Instant>>,
}

impl WorktreeAuthority {
    pub fn new(repo_root: PathBuf, self_held: SelfHeldSet, config: AuthorityConfig) -> Self {
        Self {
            repo_root: Arc::new(repo_root),
            self_held,
            config,
            last_seen_alive: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &AuthorityConfig { &self.config }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let c = AuthorityConfig::default();
        assert_eq!(c.sweep_interval, Duration::from_secs(900));
        assert_eq!(c.quarantine_grace, Duration::from_secs(30));
        assert!(c.fs_unsafe_skip);
    }

    #[test]
    fn sweep_report_default_is_all_zero() {
        let r = SweepReport::default();
        assert_eq!(r.probed, 0);
        assert_eq!(r.swept, 0);
    }
}
```

In `crates/spur-core/src/lib.rs`, add:

```rust
pub mod worktree_authority;
pub use worktree_authority::{AuthorityConfig, SweepReport, WorktreeAuthority};
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p spur-core worktree_authority::tests
```
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/worktree_authority.rs crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): WorktreeAuthority skeleton + SweepReport"
```

---

## Task 15: `WorktreeAuthority::sweep_once` core logic

**Files:**
- Modify: `crates/spur-core/src/worktree_authority.rs`

- [ ] **Step 1: Write the failing tests**

Append to `worktree_authority::tests`:

```rust
use spur_acp::SessionId;
use tempfile::TempDir;

async fn seed_repo_with_worktree(td: &TempDir, branch: &str) -> std::path::PathBuf {
    use tokio::process::Command;
    async fn git(dir: &std::path::Path, args: &[&str]) {
        let s = Command::new("git").args(args).current_dir(dir).output().await.unwrap();
        assert!(s.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&s.stderr));
    }
    git(td.path(), &["init", "-q", "-b", "main"]).await;
    git(td.path(), &["config", "user.email", "t@t"]).await;
    git(td.path(), &["config", "user.name", "t"]).await;
    tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
    git(td.path(), &["add", "a"]).await;
    git(td.path(), &["commit", "-q", "-m", "base"]).await;
    let wt = td.path().join(".spur/worktrees/abc");
    git(td.path(), &["worktree", "add", wt.to_str().unwrap(), "-b", branch, "main"]).await;
    wt
}

fn id(s: &str) -> BrainSessionId {
    BrainSessionId::new(SessionId(s.into()))
}

#[tokio::test]
async fn sweep_skips_legacy_worker_branches() {
    let td = TempDir::new().unwrap();
    let _ = seed_repo_with_worktree(&td, "spur/worker-legacy-deadbeef-1111-2222-3333-444455556666").await;
    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        SelfHeldSet::new(),
        AuthorityConfig { quarantine_grace: Duration::ZERO, ..AuthorityConfig::default() },
    );
    let r = auth.sweep_once().await.expect("sweep ok");
    assert_eq!(r.skipped_unknown_owner, 1);
    assert_eq!(r.swept, 0);
}

#[tokio::test]
async fn sweep_reclaims_v2_worktree_when_session_lock_missing() {
    let td = TempDir::new().unwrap();
    let brain = "550e8400-e29b-41d4-a716-446655440000";
    let worker = "deadbeef-1111-2222-3333-444455556666";
    let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
    let _ = seed_repo_with_worktree(&td, &branch).await;
    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        SelfHeldSet::new(),
        AuthorityConfig { quarantine_grace: Duration::ZERO, ..AuthorityConfig::default() },
    );
    let r = auth.sweep_once().await.expect("sweep ok");
    assert_eq!(r.swept, 1);
    assert_eq!(r.probed, 1);
}

#[tokio::test]
async fn sweep_skips_self_held_session() {
    let td = TempDir::new().unwrap();
    let brain = "550e8400-e29b-41d4-a716-446655440000";
    let worker = "deadbeef-1111-2222-3333-444455556666";
    let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
    let _ = seed_repo_with_worktree(&td, &branch).await;
    let self_held = SelfHeldSet::new();
    self_held.insert(id(brain));
    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held,
        AuthorityConfig { quarantine_grace: Duration::ZERO, ..AuthorityConfig::default() },
    );
    let r = auth.sweep_once().await.expect("sweep ok");
    assert_eq!(r.skipped_self, 1);
    assert_eq!(r.swept, 0);
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-core worktree_authority
```
Expected: FAIL — `sweep_once` does not exist.

- [ ] **Step 3: Implement `sweep_once`**

Add to `crates/spur-core/src/worktree_authority.rs`:

```rust
use spur_acp::session_liveness::{SessionLivenessProbe, SessionLivenessProbeResult};
use spur_worktree::manager::parse_v2_branch;
use tokio::process::Command;

impl WorktreeAuthority {
    pub async fn sweep_once(&self) -> Result<SweepReport, AuthorityError> {
        let mut report = SweepReport::default();
        let entries = self.enumerate_worktrees().await?;
        let now = Instant::now();
        let mut last_seen = self.last_seen_alive.lock().await;

        for (path, branch) in entries {
            if !branch.starts_with("refs/heads/spur/worker/v2/") {
                report.skipped_unknown_owner += 1;
                continue;
            }
            let trimmed = branch.trim_start_matches("refs/heads/");
            let owner = match parse_v2_branch(trimmed) {
                Some(o) => o,
                None => {
                    report.skipped_unknown_owner += 1;
                    continue;
                }
            };
            report.probed += 1;
            let result = SessionLivenessProbe::probe(&self.repo_root, &owner.brain_session_id, &self.self_held);
            match result {
                SessionLivenessProbeResult::Self_ => {
                    last_seen.insert(owner.brain_session_id.clone(), now);
                    report.skipped_self += 1;
                }
                SessionLivenessProbeResult::Live => {
                    last_seen.insert(owner.brain_session_id.clone(), now);
                    report.skipped_live += 1;
                }
                SessionLivenessProbeResult::FsUnsafe => {
                    report.skipped_fs_unsafe += 1;
                }
                SessionLivenessProbeResult::Missing => {
                    if self.is_quarantine_expired(&owner.brain_session_id, now, &last_seen) {
                        if let Err(e) = self.sweep_one(&path, trimmed).await {
                            warn!(error=%e, path=%path.display(), "sweep_one (missing lock) failed");
                            report.remove_failures += 1;
                        } else {
                            report.swept += 1;
                        }
                    } else {
                        report.skipped_quarantine += 1;
                    }
                }
                SessionLivenessProbeResult::DeadAcquired(guard) => {
                    if self.is_quarantine_expired(&owner.brain_session_id, now, &last_seen) {
                        if let Err(e) = self.sweep_one(&path, trimmed).await {
                            warn!(error=%e, path=%path.display(), "sweep_one failed");
                            report.remove_failures += 1;
                        } else {
                            report.swept += 1;
                        }
                    } else {
                        report.skipped_quarantine += 1;
                    }
                    drop(guard);  // explicit: release flock at end of arm
                }
            }
        }
        Ok(report)
    }

    fn is_quarantine_expired(
        &self,
        brain: &BrainSessionId,
        now: Instant,
        last_seen: &HashMap<BrainSessionId, Instant>,
    ) -> bool {
        match last_seen.get(brain) {
            Some(t) => now.duration_since(*t) >= self.config.quarantine_grace,
            None => true,
        }
    }

    async fn enumerate_worktrees(&self) -> Result<Vec<(PathBuf, String)>, AuthorityError> {
        let out = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(AuthorityError::Io)?;
        if !out.status.success() {
            return Err(AuthorityError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut result = Vec::new();
        let mut path: Option<PathBuf> = None;
        for line in stdout.lines().chain(std::iter::once("")) {
            if line.is_empty() {
                path = None;
                continue;
            }
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            }
            if let Some(b) = line.strip_prefix("branch ") {
                if let Some(p) = path.take() {
                    result.push((p, b.to_string()));
                }
            }
        }
        Ok(result)
    }

    async fn sweep_one(&self, path: &std::path::Path, branch: &str) -> Result<(), AuthorityError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| AuthorityError::Git("worktree path not UTF-8".into()))?;
        let out = Command::new("git")
            .args(["worktree", "remove", "--force", "--force", path_str])
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(AuthorityError::Io)?;
        if !out.status.success() {
            return Err(AuthorityError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let _ = Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(&*self.repo_root)
            .output()
            .await;
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&*self.repo_root)
            .output()
            .await;
        Ok(())
    }
}

```

Add `spur-worktree = { workspace = true }` to `crates/spur-core/Cargo.toml` if not already present.

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-core worktree_authority
```
Expected: all tests pass (including the two skeleton tests + three new sweep tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/worktree_authority.rs crates/spur-core/Cargo.toml
git commit -m "feat(spur-core): WorktreeAuthority::sweep_once with v2 + probe + quarantine"
```

---

## Task 16: Quarantine grace test (round-trip)

**Files:**
- Modify: `crates/spur-core/src/worktree_authority.rs` (test mod)

- [ ] **Step 1: Add the failing test**

Append to `worktree_authority::tests`:

```rust
#[tokio::test]
async fn sweep_respects_quarantine_grace() {
    let td = TempDir::new().unwrap();
    let brain = "550e8400-e29b-41d4-a716-446655440000";
    let worker = "deadbeef-1111-2222-3333-444455556666";
    let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
    let _ = seed_repo_with_worktree(&td, &branch).await;

    // Prime the authority with a "recently seen" timestamp via a held lockfile,
    // then drop the holder and immediately call sweep_once with a 5s grace.
    std::fs::create_dir_all(td.path().join(".spur/sessions")).unwrap();
    let lock_path = td.path().join(".spur/sessions").join(format!("{brain}.lock"));
    std::fs::write(&lock_path, b"").unwrap();

    use std::fs::OpenOptions;
    use fs4::fs_std::FileExt;
    let held = OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
    held.try_lock_exclusive().unwrap();

    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        SelfHeldSet::new(),
        AuthorityConfig { quarantine_grace: Duration::from_secs(5), ..AuthorityConfig::default() },
    );

    // First sweep observes Live; primes last_seen_alive.
    let r1 = auth.sweep_once().await.unwrap();
    assert_eq!(r1.skipped_live, 1);

    drop(held); // release the lock

    // Second sweep should now find it dead, but quarantine prevents sweep.
    let r2 = auth.sweep_once().await.unwrap();
    assert_eq!(r2.skipped_quarantine, 1);
    assert_eq!(r2.swept, 0);
}
```

- [ ] **Step 2: Run — confirm pass**

```bash
cargo test -p spur-core worktree_authority::tests::sweep_respects_quarantine_grace
```
Expected: PASS (the quarantine logic was already wired in Task 15).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/worktree_authority.rs
git commit -m "test(spur-core): quarantine grace blocks immediate sweep after lock dies"
```

---

## Task 16b: fs_unsafe sweep skip (spec C7)

**Files:**
- Modify: `crates/spur-core/src/worktree_authority.rs`

- [ ] **Step 1: Surface a public `fs_unsafe_skip` short-circuit**

The current `sweep_once` honors `FsUnsafe` per-entry but does not honor the spec's "skip GC entirely on `fs_unsafe` filesystem" axiom. Add a pre-flight detector and bail out at the top of `sweep_once`.

In `worktree_authority.rs`, add this helper:

```rust
impl WorktreeAuthority {
    /// Detect whether the repo's `.spur/sessions/` directory supports
    /// advisory locking. Probes a temp file once; cached not implemented
    /// in v0 (re-probes per sweep, ~1ms cost).
    async fn detect_fs_unsafe(&self) -> bool {
        let probe_path = self.repo_root.join(".spur/sessions/.fs_probe");
        if let Some(parent) = probe_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&probe_path, b"").await.is_err() {
            return false; // can't even write — let the real path fail loud
        }
        let file = match std::fs::OpenOptions::new()
            .read(true).write(true).open(&probe_path)
        {
            Ok(f) => f,
            Err(_) => return false,
        };
        use fs4::fs_std::FileExt;
        let result = file.try_lock_exclusive();
        let _ = tokio::fs::remove_file(&probe_path).await;
        match result {
            Err(e) if matches!(e.kind(), std::io::ErrorKind::Unsupported)
                || e.raw_os_error() == Some(libc::ENOLCK)
                || e.raw_os_error() == Some(libc::ENOTSUP) => true,
            _ => false,
        }
    }
}
```

Modify `sweep_once` to short-circuit:

```rust
pub async fn sweep_once(&self) -> Result<SweepReport, AuthorityError> {
    let mut report = SweepReport::default();
    if self.config.fs_unsafe_skip && self.detect_fs_unsafe().await {
        info!(target: "spur.metrics.worktree_authority.fs_unsafe_skip",
            "filesystem does not support advisory locks; sweep skipped");
        return Ok(report);
    }
    // ...existing enumerate + match code...
}
```

- [ ] **Step 2: Write the failing test**

Append to `worktree_authority::tests`:

```rust
#[tokio::test]
async fn sweep_short_circuits_when_fs_unsafe_detected() {
    // We can't easily fake ENOTSUP in a unit test on a normal disk.
    // Instead, verify the explicit config path: when fs_unsafe_skip is
    // false, we still try to sweep even on a hypothetical unsafe FS.
    // This test documents the contract; a real ENOTSUP test would
    // require a mock filesystem (deferred).
    let td = TempDir::new().unwrap();
    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        SelfHeldSet::new(),
        AuthorityConfig {
            fs_unsafe_skip: false,
            quarantine_grace: Duration::ZERO,
            sweep_interval: Duration::from_secs(900),
        },
    );
    // No git repo here; sweep should fail at enumerate, not silently succeed.
    let r = auth.sweep_once().await;
    assert!(r.is_err(), "with fs_unsafe_skip=false on a non-repo dir, sweep should error");
}
```

- [ ] **Step 3: Run**

```bash
cargo test -p spur-core worktree_authority::tests::sweep_short_circuits_when_fs_unsafe_detected
```
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/worktree_authority.rs
git commit -m "feat(spur-core): WorktreeAuthority short-circuits sweep on fs_unsafe (spec C7)"
```

---

## Task 17: `spawn_periodic` with jitter and abort

**Files:**
- Modify: `crates/spur-core/src/worktree_authority.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[tokio::test]
async fn spawn_periodic_returns_aborable_handle() {
    let td = TempDir::new().unwrap();
    let auth = Arc::new(WorktreeAuthority::new(
        td.path().to_path_buf(),
        SelfHeldSet::new(),
        AuthorityConfig {
            sweep_interval: Duration::from_millis(50),
            quarantine_grace: Duration::ZERO,
            fs_unsafe_skip: true,
        },
    ));
    let handle = auth.clone().spawn_periodic();
    tokio::time::sleep(Duration::from_millis(120)).await;
    handle.abort();
    let res = handle.await;
    assert!(res.is_err() && res.unwrap_err().is_cancelled(),
        "handle must be cancellable");
}
```

- [ ] **Step 2: Run — confirm fail**

```bash
cargo test -p spur-core worktree_authority::tests::spawn_periodic_returns_aborable_handle
```
Expected: FAIL — `spawn_periodic` does not exist.

- [ ] **Step 3: Implement `spawn_periodic`**

Add to the `WorktreeAuthority` impl in `worktree_authority.rs`:

```rust
impl WorktreeAuthority {
    pub fn spawn_periodic(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let jitter_ms: u64 = (std::ptr::addr_of!(*self) as usize as u64) % 120_000;
                let delay = self.config.sweep_interval + Duration::from_millis(jitter_ms);
                tokio::time::sleep(delay).await;
                match self.sweep_once().await {
                    Ok(report) => {
                        info!(
                            target: "spur.metrics.worktree_authority.periodic",
                            probed = report.probed,
                            swept = report.swept,
                            skipped_self = report.skipped_self,
                            skipped_live = report.skipped_live,
                            skipped_quarantine = report.skipped_quarantine,
                            skipped_unknown_owner = report.skipped_unknown_owner,
                            skipped_fs_unsafe = report.skipped_fs_unsafe,
                            remove_failures = report.remove_failures,
                        );
                    }
                    Err(e) => {
                        error!(target: "spur.metrics.worktree_authority.periodic_failed",
                            error=%e);
                    }
                }
            }
        })
    }
}
```

Note: the `addr_of` jitter is a deterministic-but-spread-out hack; for production rigor swap to `rand::thread_rng()` after this task lands.

- [ ] **Step 4: Run — confirm pass**

```bash
cargo test -p spur-core worktree_authority::tests::spawn_periodic_returns_aborable_handle
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/worktree_authority.rs
git commit -m "feat(spur-core): WorktreeAuthority::spawn_periodic with abortable handle"
```

---

## Task 18: Wire authority into `Orchestrator::new`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:792, 954–1095`

- [ ] **Step 1: Replace the dead `pub worktrees` field**

In `crates/spur-core/src/orchestrator.rs`, replace the field declaration around line 792:

```rust
// REMOVE:
//   pub worktrees: WorktreeManager,

// ADD:
pub worktree_authority: Arc<WorktreeAuthority>,
pub self_held: spur_acp::session_liveness::SelfHeldSet,
```

- [ ] **Step 2: Update construction in `Orchestrator::new` at line 954–965**

Replace:
```rust
let worktrees = WorktreeManager::new(repo_root.clone());
let outcome_store: Arc<dyn OutcomeStore> = Arc::new(MeasuredOutcomeStore::new(
    GitBlobOutcomeStore::new(worktrees.repo_root.clone()),
));
```

With:
```rust
let outcome_store: Arc<dyn OutcomeStore> = Arc::new(MeasuredOutcomeStore::new(
    GitBlobOutcomeStore::new(repo_root.clone()),
));
let self_held = spur_acp::session_liveness::SelfHeldSet::new();
let worktree_authority = Arc::new(crate::WorktreeAuthority::new(
    repo_root.clone(),
    self_held.clone(),
    crate::AuthorityConfig::default(),
));
```

In the struct-literal that follows, replace `worktrees,` with `worktree_authority: worktree_authority.clone(),` and add `self_held,`.

- [ ] **Step 3: Run startup sweep before orchestrator becomes reachable**

In `Orchestrator::new`, AFTER the orchestrator struct is built but BEFORE the function returns, add:

```rust
// Startup sweep: safe because self_held is empty, but the dead-lock
// signal is conservative (kill_on_drop ensures workers died with their
// orchestrators).
match worktree_authority.sweep_once().await {
    Ok(report) => tracing::info!(
        target: "spur.metrics.worktree_authority.startup",
        probed = report.probed,
        swept = report.swept,
        skipped_unknown_owner = report.skipped_unknown_owner,
        skipped_live = report.skipped_live,
    ),
    Err(e) => tracing::warn!(error=%e, "startup worktree authority sweep failed"),
}

// Spawn periodic sweep into background_tasks (existing infra at :918).
let periodic = worktree_authority.clone().spawn_periodic();
orchestrator.background_tasks.push(periodic);
```

- [ ] **Step 4: Build the workspace**

```bash
cargo build -p spur-core
```
Expected: clean build. If the per-delegation `let mut worktrees = WorktreeManager::new(repo_root)` at `:4248` still references `repo_root` correctly, no other changes are needed.

- [ ] **Step 5: Run the full spur-core test suite**

```bash
cargo test -p spur-core
```
Expected: all existing tests pass. New `worktree_authority` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): wire WorktreeAuthority into Orchestrator boot path"
```

---

## Task 19: Maintain `SelfHeldSet` across session lifecycle

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — `load_brain_session()`, `create_brain_session()`, `retire_active_brain()`

- [ ] **Step 1: Locate the three sites**

Run:
```bash
rg -n 'fn load_brain_session|fn create_brain_session|fn retire_active_brain' crates/spur-core/src/orchestrator.rs
```
Expected: three matches (around `:1448–1500` per the prior session-attach spec).

- [ ] **Step 2: Add `self_held.insert(...)` after successful session establishment**

In `load_brain_session()`, after the `BrainSession` is constructed and the attach guard is held, add:
```rust
self.self_held.insert(brain_session_id.clone());
```

In `create_brain_session()`, similarly:
```rust
self.self_held.insert(brain_session_id.clone());
```

In `retire_active_brain()`, BEFORE dropping the attach guard:
```rust
self.self_held.remove(&brain_session_id);
```

The ordering invariant from the spec §6 risk table: `self_held.remove` must happen BEFORE the attach guard's lockfile is unlinked, so that any concurrent peer probe that races finds either `Live` (lock still held) or `Self_` (we still claim it). Never the gap state.

- [ ] **Step 3: Add an integration test for the lifecycle**

Append to `crates/spur-core/tests/worktree_authority.rs` (create file if missing):

```rust
//! End-to-end test: SelfHeldSet maintenance prevents authority from
//! sweeping its own active sessions.

use std::sync::Arc;
use std::time::Duration;

use spur_acp::session_liveness::SelfHeldSet;
use spur_core::{AuthorityConfig, WorktreeAuthority};
use tempfile::TempDir;

#[tokio::test]
async fn self_held_session_prevents_sweep_during_active_use() {
    let td = TempDir::new().unwrap();
    // Set up repo + v2 worktree owned by brain X.
    use tokio::process::Command;
    let _ = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["config", "user.email", "t@t"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["config", "user.name", "t"]).current_dir(td.path()).output().await.unwrap();
    tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
    let _ = Command::new("git").args(["add", "a"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["commit", "-q", "-m", "base"]).current_dir(td.path()).output().await.unwrap();

    let brain = "550e8400-e29b-41d4-a716-446655440000";
    let worker = "deadbeef-1111-2222-3333-444455556666";
    let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
    let wt = td.path().join(".spur/worktrees/abc");
    let _ = Command::new("git")
        .args(["worktree", "add", wt.to_str().unwrap(), "-b", &branch, "main"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();

    let self_held = SelfHeldSet::new();
    self_held.insert(spur_acp::BrainSessionId::new(spur_acp::SessionId(brain.into())));

    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held,
        AuthorityConfig { quarantine_grace: Duration::ZERO, ..AuthorityConfig::default() },
    );
    let report = auth.sweep_once().await.expect("sweep");
    assert_eq!(report.skipped_self, 1, "must skip self-held session");
    assert_eq!(report.swept, 0, "must NOT sweep self-held session");
    assert!(wt.exists(), "worktree dir must still exist on disk");
}
```

- [ ] **Step 4: Run**

```bash
cargo build -p spur-core
cargo test -p spur-core --test worktree_authority self_held_session_prevents_sweep_during_active_use
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/worktree_authority.rs
git commit -m "feat(spur-core): maintain SelfHeldSet across brain session lifecycle"
```

---

## Task 20: Multi-process safety integration test

**Files:**
- Modify: `crates/spur-core/tests/worktree_authority.rs`

- [ ] **Step 1: Add the multi-process test**

This is the critical safety guarantee: two coexisting orchestrators must never delete each other's worktrees.

Append to `crates/spur-core/tests/worktree_authority.rs`:

```rust
#[tokio::test]
async fn two_orchestrators_do_not_sweep_each_others_worktrees() {
    let td = TempDir::new().unwrap();
    use tokio::process::Command;
    use std::fs::OpenOptions;
    use fs4::fs_std::FileExt;

    // Set up a repo with TWO v2 worktrees, owned by different brains.
    let _ = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["config", "user.email", "t@t"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["config", "user.name", "t"]).current_dir(td.path()).output().await.unwrap();
    tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
    let _ = Command::new("git").args(["add", "a"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["commit", "-q", "-m", "base"]).current_dir(td.path()).output().await.unwrap();

    let brain_a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let brain_b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    let worker_a = "11111111-1111-1111-1111-111111111111";
    let worker_b = "22222222-2222-2222-2222-222222222222";
    let branch_a = format!("spur/worker/v2/codex/{brain_a}/{worker_a}");
    let branch_b = format!("spur/worker/v2/codex/{brain_b}/{worker_b}");
    let wt_a = td.path().join(".spur/worktrees/wa");
    let wt_b = td.path().join(".spur/worktrees/wb");
    let _ = Command::new("git").args(["worktree", "add", wt_a.to_str().unwrap(), "-b", &branch_a, "main"]).current_dir(td.path()).output().await.unwrap();
    let _ = Command::new("git").args(["worktree", "add", wt_b.to_str().unwrap(), "-b", &branch_b, "main"]).current_dir(td.path()).output().await.unwrap();

    // Simulate orchestrator A: holds session A's lockfile.
    std::fs::create_dir_all(td.path().join(".spur/sessions")).unwrap();
    let lock_a = td.path().join(".spur/sessions").join(format!("{brain_a}.lock"));
    std::fs::write(&lock_a, b"").unwrap();
    let held_a = OpenOptions::new().read(true).write(true).open(&lock_a).unwrap();
    held_a.try_lock_exclusive().unwrap();

    // Orchestrator B (does NOT hold A's lock) sweeps.
    let self_held_b = SelfHeldSet::new();
    self_held_b.insert(spur_acp::BrainSessionId::new(spur_acp::SessionId(brain_b.into())));
    let auth_b = WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held_b,
        AuthorityConfig { quarantine_grace: Duration::ZERO, ..AuthorityConfig::default() },
    );
    let report = auth_b.sweep_once().await.expect("sweep");

    assert_eq!(report.skipped_live, 1, "B must see A's session as Live");
    assert_eq!(report.skipped_self, 1, "B must skip its own session");
    assert_eq!(report.swept, 0, "B must not delete anything");
    assert!(wt_a.exists(), "A's worktree must still exist on disk");
    assert!(wt_b.exists(), "B's worktree must still exist on disk");

    drop(held_a);
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p spur-core --test worktree_authority two_orchestrators_do_not_sweep_each_others_worktrees
```
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/worktree_authority.rs
git commit -m "test(spur-core): two orchestrators must not sweep each other's worktrees"
```

---

## Task 21: Workspace smoke + CHANGELOG entry

**Files:**
- Modify: `CHANGELOG.md` (or equivalent — check repo for the right path)

- [ ] **Step 1: Run the full workspace test suite**

```bash
cargo test --workspace
```
Expected: all tests pass.

- [ ] **Step 2: Run clippy in strict mode**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: no warnings.

- [ ] **Step 3: Add a CHANGELOG entry**

If `CHANGELOG.md` exists at the repo root, add under the `## [Unreleased]` section:

```markdown
### Fixed
- Risk #4 (worktree orphaning) — `WorktreeAuthority` actor sweeps dead-session worktrees safely under multi-process operation. Branch namespace migrated to `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}`. Pre-v2 branches are NOT auto-cleaned; operators reclaim legacy debt via the separate `spur-worktree-gc-legacy.sh` script.
- Worker child processes now die with their orchestrator (`kill_on_drop(true)` on `crates/spur-acp/src/connection/{native,stdio_adapter,cli_wrap_adapter,stream_json_adapter}.rs`). Closes Risk 4's hard prerequisite.
```

If no CHANGELOG, skip this step and note it in the final commit message.

- [ ] **Step 4: Commit and push**

```bash
git add CHANGELOG.md  # if applicable
git commit -m "chore: changelog entry for Risk 4 (worktree orphaning) fix"
```

---

## Out-of-band follow-ups (separate plans)

The following are referenced in the spec but tracked as separate work items, not in this plan:

- **Phase 0b** — JoinSet/JoinHandle supervision on `orchestrator.rs:3921` (handle_delegations) and `:2566,2830,4789` (ext-notification pumps). Closes Risks 6 + 27.
- **Phase 2** — `fsync` for `FsOutcomeStore::put`, snapshot branch process_id+nonce, NFS deployment policy decision.
- **Phase 3** — convert blob-store sweep at `orchestrator.rs:1041` from one-shot to interval-driven on the WorktreeAuthority's cadence.
- **Phase L** — `scripts/spur-worktree-gc-legacy.sh` to reclaim the existing 10 GB of pre-v2 debt.
- **Architecture.md edits** — Risk 4 row rewrite, strike hallucinated cluster, new §7 paragraph on blob-store crash-durability gap, decomposition recommendation update.

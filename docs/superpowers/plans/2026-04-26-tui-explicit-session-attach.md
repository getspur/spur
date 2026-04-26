# TUI Explicit Session Attach Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce the single-attach invariant for SPUR sessions via an `fs4` advisory lockfile and replace implicit auto-resume with an explicit picker-landing flow.

**Architecture:** Phase 1a does a mechanical tuple→struct refactor on `Orchestrator::agent_connection`. Phase 1b adds a `SessionAttachGuard` whose lifetime is tied to that struct, plus structured `SessionAttachRejected` events that surface to a TUI modal. Phase 2 reroutes `LandingDecision::AutoResume` to land on the existing `SessionPickerView` with the last session preselected, and adds a `--session <id>` flag for explicit attach.

**Tech Stack:** Rust workspace · `fs4 = "0.13"` (cross-platform advisory locks) · `serde_json` (lockfile content) · `chrono` (timestamps) · ratatui (TUI) · existing `tokio` async runtime · existing `insta`/`cargo test` test infra.

**Spec:** [`docs/superpowers/specs/2026-04-26-tui-explicit-session-attach-design.md`](../specs/2026-04-26-tui-explicit-session-attach-design.md)

---

## File Structure

**Files to create:**
- `crates/spur-acp/src/session_lock.rs` — `SessionAttachGuard`, `HolderInfo`, `AcquireOutcome`, `try_acquire()` + unit tests
- `crates/spur-tui/src/components/collision_modal.rs` — centered popup mirroring `quit_confirm.rs`
- `crates/spur-cli/tests/session_attach_collision.rs` — cross-process integration test

**Files to modify:**
- `crates/spur-acp/Cargo.toml` — add `fs4`
- `crates/spur-acp/src/lib.rs` — export `session_lock` module
- `crates/spur-acp/src/domain/events.rs` — add `SessionAttachRejected` variant; add `fs_unsafe: bool` to `AgentSessionReady`
- `crates/spur-core/src/orchestrator.rs` — `ActiveConnection` struct (Phase 1a); lock acquisition + event emission (Phase 1b); `LoadBrainSessionError::AlreadyAttached`
- `crates/spur-tui/src/components/mod.rs` — register `collision_modal`
- `crates/spur-tui/src/app.rs` — handle `SessionAttachRejected`, open modal; handle `AgentSessionReady { fs_unsafe: true }`, set state flag
- `crates/spur-tui/src/views/session_detail.rs` — render `fs_unsafe` banner + header tag
- `crates/spur-tui/src/landing.rs` — add `AttachExplicit { acp_id, brain }`; add `preselect: Option<String>` to `ShowPicker`
- `crates/spur-cli/src/main.rs` — add `--session` clap flag; update `resolve_landing()` (line 56); update CLI dispatch (lines 717-738)
- `crates/spur-tui/src/views/session_picker.rs` — add `preselect: Option<String>` field; render banner; jump cursor on first render

---

## Phase 1a — `ActiveConnection` Named Struct (Mechanical Refactor)

### Task 1: Introduce `ActiveConnection` struct in orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (near `BrainSession` struct definition, around line 143)

- [ ] **Step 1: Add the struct definition just above `pub struct BrainSession`**

```rust
/// Holds the active brain transport along with metadata that must
/// share its lifetime. Future fields (e.g. SessionAttachGuard) are
/// added here so they cannot accidentally outlive the connection.
pub struct ActiveConnection {
    pub transport: Box<dyn AgentConnection>,
    pub brain_name: String,
}
```

- [ ] **Step 2: Change the field type**

Locate the orchestrator's `agent_connection: Option<(Box<dyn AgentConnection>, String)>` declaration (search the file for `agent_connection:`) and change to:

```rust
agent_connection: Option<ActiveConnection>,
```

- [ ] **Step 3: Run `cargo check -p spur-core`**

Expected: many compile errors about tuple vs struct destructuring at the 7 known sites (1342, 1399, 1444, 1459, 1828, 2128, 2366) plus any others rustc finds.

### Task 2: Update all `agent_connection` access sites

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` lines 1342, 1399, 1444, 1459, 1828, 2128, 2366 (and any others rustc surfaces)

- [ ] **Step 1: Update line 1342 (assign Some)**

Before:
```rust
agent_connection = Some((conn, brain_name.clone()));
```
After:
```rust
agent_connection = Some(ActiveConnection {
    transport: conn,
    brain_name: brain_name.clone(),
});
```

- [ ] **Step 2: Update line 1399 (destructure on take)**

Before:
```rust
let (mut conn, brain_name) = match agent_connection.take() { ... };
```
After:
```rust
let ActiveConnection { transport: mut conn, brain_name } =
    match agent_connection.take() { ... };
```

- [ ] **Step 3: Update line 1444 (assign Some)**

Before:
```rust
agent_connection = Some((conn, brain_name));
```
After:
```rust
agent_connection = Some(ActiveConnection { transport: conn, brain_name });
```

- [ ] **Step 4: Update line 1459 (destructure on take)**

Same pattern as Step 2.

- [ ] **Step 5: Update line 1828 (`let result = match agent_connection.take()`)**

Replace tuple bindings with struct field bindings (`let ActiveConnection { transport, brain_name } = ...`).

- [ ] **Step 6: Update line 2128 (`if let Some((mut conn, _)) = agent_connection.take()`)**

After:
```rust
if let Some(ActiveConnection { transport: mut conn, .. }) = agent_connection.take() {
```

- [ ] **Step 7: Update line 2366 in `retire_active_brain` (`*agent_connection = Some((b.connection, b.brain_name))`)**

After:
```rust
*agent_connection = Some(ActiveConnection {
    transport: b.connection,
    brain_name: b.brain_name,
});
```

- [ ] **Step 8: Run `cargo check -p spur-core`**

Expected: clean. If rustc points to additional sites not in the list above, update them with the same pattern (tuple → struct field destructure).

### Task 3: Run all workspace tests + commit

- [ ] **Step 1: Run `cargo test --workspace --no-fail-fast`**

Expected: all green. The refactor is behavior-preserving.

- [ ] **Step 2: Commit Phase 1a**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "refactor(spur-core): introduce ActiveConnection struct (phase 1a)

Replaces the (Box<dyn AgentConnection>, String) tuple with a named
struct. No behavior change. Prepares the field for the SessionAttachGuard
that ties lock lifetime to transport lifetime in phase 1b.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 1b — Lock Module + Events + TUI Integration

### Task 4: Add `fs4` dependency

**Files:**
- Modify: `crates/spur-acp/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `[dependencies]`, add:
```toml
fs4 = "0.13"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo check -p spur-acp`
Expected: clean (no usage yet).

### Task 5: Create `session_lock` module skeleton

**Files:**
- Create: `crates/spur-acp/src/session_lock.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Create the file with type definitions only (no logic yet)**

```rust
//! Single-attach lockfile for SPUR ACP sessions.
//!
//! Enforces the invariant that at most one orchestrator process holds an
//! active ACP attachment to a given session id. Backed by `fs4` advisory
//! locks; kernel auto-releases on process exit so no stale-lock recovery
//! is needed.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolderInfo {
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub tty: Option<String>,
    pub label: Option<String>,
    pub workdir: Option<PathBuf>,
}

pub struct SessionAttachGuard {
    file: std::fs::File,
    pid_path: PathBuf,
    acp_id: String,
}

pub enum AcquireOutcome {
    /// Exclusive ownership; proceed.
    Acquired(SessionAttachGuard),
    /// Filesystem rejected advisory locking (NFS/sshfs/SMB ENOTSUP/ENOLCK).
    /// Caller should attach with `fs_unsafe = true`.
    DegradedNoLock { reason: String },
    /// Another process holds it.
    Rejected { holder: HolderInfo },
    /// Unrecoverable IO error (permissions, disk full, etc.).
    Io(std::io::Error),
}

impl SessionAttachGuard {
    pub fn try_acquire(repo_root: &Path, acp_id: &str) -> AcquireOutcome {
        unimplemented!("Task 6")
    }
}

impl Drop for SessionAttachGuard {
    fn drop(&mut self) {
        // Best-effort cleanup; kernel releases the flock on file close.
        let _ = std::fs::remove_file(&self.pid_path);
    }
}
```

- [ ] **Step 2: Export from `lib.rs`**

Add to `crates/spur-acp/src/lib.rs`:
```rust
pub mod session_lock;
```

- [ ] **Step 3: Run `cargo check -p spur-acp`**

Expected: clean (skeleton compiles; `try_acquire` body is `unimplemented!`).

### Task 6: TDD — `try_acquire` Acquired path

**Files:**
- Modify: `crates/spur-acp/src/session_lock.rs`

- [ ] **Step 1: Write the failing test at the bottom of the file**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_then_release_drops_lock() {
        let tmp = TempDir::new().unwrap();
        match SessionAttachGuard::try_acquire(tmp.path(), "test-session") {
            AcquireOutcome::Acquired(guard) => {
                drop(guard);
                // Re-acquire should succeed after Drop releases the flock.
                match SessionAttachGuard::try_acquire(tmp.path(), "test-session") {
                    AcquireOutcome::Acquired(_) => {}
                    other => panic!("expected Acquired after release, got {:?}",
                        std::mem::discriminant(&other)),
                }
            }
            other => panic!("expected Acquired, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
```

Add `tempfile = "3"` to `[dev-dependencies]` in `crates/spur-acp/Cargo.toml` if not already present.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-acp session_lock::tests::acquire_then_release_drops_lock`
Expected: FAIL with `unimplemented!("Task 6")` panic.

- [ ] **Step 3: Implement `try_acquire`**

Replace the body of `try_acquire` with:

```rust
pub fn try_acquire(repo_root: &Path, acp_id: &str) -> AcquireOutcome {
    use fs4::fs_std::FileExt;

    let dir = repo_root.join(".spur").join("sessions");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return AcquireOutcome::Io(e);
    }
    let pid_path = dir.join(format!("{acp_id}.attach.lock"));

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&pid_path)
    {
        Ok(f) => f,
        Err(e) => return AcquireOutcome::Io(e),
    };

    match file.try_lock_exclusive() {
        Ok(true) => {
            // Write our PID record after acquiring the lock.
            let info = HolderInfo {
                pid: Some(std::process::id()),
                started_at: Some(Utc::now()),
                tty: detect_tty(),
                label: std::env::var("SPUR_TUI_LABEL").ok(),
                workdir: std::env::current_dir().ok(),
            };
            let _ = write_holder_info(&pid_path, &info); // best-effort

            AcquireOutcome::Acquired(SessionAttachGuard {
                file,
                pid_path,
                acp_id: acp_id.to_string(),
            })
        }
        Ok(false) => {
            // Another holder; read whatever they wrote (best-effort).
            let holder = read_holder_info(&pid_path).unwrap_or_default();
            AcquireOutcome::Rejected { holder }
        }
        Err(e) => classify_lock_error(e, &pid_path),
    }
}

fn detect_tty() -> Option<String> {
    // Phase 3 polish; return None for now.
    None
}

fn write_holder_info(path: &Path, info: &HolderInfo) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(path)?;
    f.set_len(0)?;  // CRITICAL: prevents stale-PID-with-trailing-junk
    let json = serde_json::to_string(info).unwrap_or_else(|_| "{}".into());
    f.write_all(json.as_bytes())?;
    Ok(())
}

fn read_holder_info(path: &Path) -> Option<HolderInfo> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn classify_lock_error(e: std::io::Error, _path: &Path) -> AcquireOutcome {
    use std::io::ErrorKind;
    // ENOTSUP / ENOLCK signal a filesystem that cannot do flock.
    let raw = e.raw_os_error();
    let is_unsupported = raw == Some(libc::ENOTSUP)
        || raw == Some(libc::ENOLCK)
        || matches!(e.kind(), ErrorKind::Unsupported);
    if is_unsupported {
        AcquireOutcome::DegradedNoLock {
            reason: format!("flock unsupported on volume: {e}"),
        }
    } else {
        AcquireOutcome::Io(e)
    }
}
```

Add to top of file: `use chrono::Utc;` (already present from earlier import).
Add to `[dependencies]` in `crates/spur-acp/Cargo.toml` if missing: `serde_json = "1"`, `libc = "0.2"` (Windows uses different error codes — see Task 8 for the cross-platform classifier).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-acp session_lock::tests::acquire_then_release_drops_lock`
Expected: PASS.

### Task 7: TDD — Concurrent acquire returns Rejected with HolderInfo

**Files:**
- Modify: `crates/spur-acp/src/session_lock.rs` (tests module)

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn concurrent_acquire_in_same_process_returns_rejected_with_pid() {
    let tmp = TempDir::new().unwrap();
    let first = match SessionAttachGuard::try_acquire(tmp.path(), "shared") {
        AcquireOutcome::Acquired(g) => g,
        other => panic!("expected first Acquired, got {:?}", std::mem::discriminant(&other)),
    };
    match SessionAttachGuard::try_acquire(tmp.path(), "shared") {
        AcquireOutcome::Rejected { holder } => {
            assert_eq!(holder.pid, Some(std::process::id()));
            assert!(holder.started_at.is_some(), "started_at should be populated");
        }
        other => panic!("expected Rejected, got {:?}", std::mem::discriminant(&other)),
    }
    drop(first);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-acp session_lock::tests::concurrent_acquire_in_same_process_returns_rejected_with_pid`
Expected: PASS (Task 6's implementation already covers this).

If FAIL with `holder.pid == None`: the JSON write/read round-trip is broken; check `write_holder_info`'s `set_len(0)` and `serde_json` invocation.

### Task 8: TDD — Cross-platform error classification

**Files:**
- Modify: `crates/spur-acp/src/session_lock.rs`

- [ ] **Step 1: Replace `classify_lock_error` with a cross-platform version**

```rust
fn classify_lock_error(e: std::io::Error, _path: &Path) -> AcquireOutcome {
    use std::io::ErrorKind;

    // Cross-platform "the lock conflict cannot be polled / unsupported on
    // this filesystem". On Linux/macOS this is ENOTSUP/ENOLCK; on Windows
    // it can surface as ErrorKind::Unsupported.
    let raw = e.raw_os_error();

    #[cfg(unix)]
    let is_unsupported = raw == Some(libc::ENOTSUP) || raw == Some(libc::ENOLCK);
    #[cfg(not(unix))]
    let is_unsupported = false;

    if is_unsupported || matches!(e.kind(), ErrorKind::Unsupported) {
        AcquireOutcome::DegradedNoLock {
            reason: format!("flock unsupported on volume: {e}"),
        }
    } else {
        AcquireOutcome::Io(e)
    }
}
```

Update `Cargo.toml` to make `libc` Unix-only:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 2: Add a unit test for the unsupported branch (Unix-only)**

```rust
#[cfg(unix)]
#[test]
fn enotsup_returns_degraded_no_lock() {
    let raw_err = std::io::Error::from_raw_os_error(libc::ENOTSUP);
    let outcome = classify_lock_error(raw_err, std::path::Path::new("/tmp/x"));
    assert!(matches!(outcome, AcquireOutcome::DegradedNoLock { .. }));
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p spur-acp session_lock::tests`
Expected: all PASS.

### Task 9: TDD — `set_len(0)` prevents stale-PID junk

**Files:**
- Modify: `crates/spur-acp/src/session_lock.rs`

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn set_len_zero_truncates_previous_holder_pid() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join(".spur").join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aaa.attach.lock");

    // Simulate a previous holder writing a long PID record.
    std::fs::write(&path, r#"{"pid":99999999,"label":"old-very-long-label-text"}"#)
        .unwrap();

    // New holder writes a shorter record.
    let info = HolderInfo {
        pid: Some(1),
        ..Default::default()
    };
    write_holder_info(&path, &info).unwrap();

    let parsed: HolderInfo =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.pid, Some(1));
    // No trailing junk: file size matches our serialized payload exactly.
    let written = serde_json::to_string(&info).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, written.len());
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-acp session_lock::tests::set_len_zero_truncates_previous_holder_pid`
Expected: PASS (Task 6's `write_holder_info` already calls `set_len(0)`).

If FAIL: confirm the implementation calls `f.set_len(0)?` BEFORE `write_all`.

### Task 10: TDD — `HolderInfo` JSON parse defaults on missing fields

**Files:**
- Modify: `crates/spur-acp/src/session_lock.rs`

- [ ] **Step 1: Add the failing test**

```rust
#[test]
fn holder_info_parses_with_only_pid_field() {
    let json = r#"{"pid": 42}"#;
    let parsed: HolderInfo = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.pid, Some(42));
    assert_eq!(parsed.started_at, None);
    assert_eq!(parsed.label, None);
}

#[test]
fn holder_info_parses_empty_object_to_all_none() {
    let json = "{}";
    let parsed: HolderInfo = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.pid, None);
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p spur-acp session_lock::tests::holder_info_parses`
Expected: PASS (the `Option<T>` fields and `#[derive(Default)]` give us this for free).

### Task 11: Commit Task 4–10

- [ ] **Step 1: Run all spur-acp tests**

Run: `cargo test -p spur-acp`
Expected: green.

- [ ] **Step 2: Commit**

```bash
git add crates/spur-acp/Cargo.toml crates/spur-acp/src/session_lock.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): SessionAttachGuard via fs4 advisory lock (phase 1b)

New session_lock module: flock-based single-attach guard, JSON-encoded
HolderInfo for diagnostics (pid + started_at + label + workdir),
graceful ENOTSUP/ENOLCK degradation for NFS/sshfs/SMB volumes, RAII
Drop releases via kernel close. set_len(0) before each PID write
prevents stale-PID-with-trailing-junk bugs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 12: Add `SessionAttachRejected` event variant

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (around line 320, inside `SpurEventBody`)

- [ ] **Step 1: Add the variant**

After the `WorkerPeerMessageConsumed` variant (or in alphabetical order if that's the convention), add:

```rust
SessionAttachRejected {
    acp_session_id: String,
    holder: crate::session_lock::HolderInfo,
    fs_unsafe: bool,
},
```

- [ ] **Step 2: Run `cargo check -p spur-acp`**

Expected: clean. If exhaustive `match` statements fail in other files, add a default arm or specific handling there (search workspace for `SpurEventBody::` exhaustive matches).

### Task 13: Add `fs_unsafe` field to `AgentSessionReady`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs:355`
- Modify: `crates/spur-acp/src/domain/events.rs:1012` (constructor)
- Modify: any other `AgentSessionReady { ... }` constructors (grep for `AgentSessionReady {`)

- [ ] **Step 1: Add the field**

```rust
AgentSessionReady {
    session: SessionId,
    acp_session_id: String,
    brain: String,
    resumed: bool,
    cancel_mode: CancelMode,
    /// True when this session was attached without an enforceable
    /// lockfile (NFS/sshfs/SMB). Multi-instance protection is OFF.
    fs_unsafe: bool,
},
```

- [ ] **Step 2: Update all constructors**

Find every `SpurEventBody::AgentSessionReady {` in the workspace:
```bash
rg -n 'SpurEventBody::AgentSessionReady\s*\{' --type rust
```

Add `fs_unsafe: false,` to each one (Task 14/15 will set it to `true` where appropriate).

- [ ] **Step 3: Run `cargo check --workspace`**

Expected: clean.

### Task 14: Add `LoadBrainSessionError::AlreadyAttached` + integrate into `load_brain_session`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (search for `LoadBrainSessionError` enum or similar; if none exists, the function returns `anyhow::Result` and we add a typed wrapper)

- [ ] **Step 1: Define the error**

If `LoadBrainSessionError` doesn't exist, add near the top of `orchestrator.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum LoadBrainSessionError {
    #[error("session {acp_id} is already attached")]
    AlreadyAttached {
        acp_id: String,
        holder: spur_acp::session_lock::HolderInfo,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

If it exists, just add the `AlreadyAttached` variant.

- [ ] **Step 2: Acquire the lock inside `load_brain_session` (around line 2614 per spec)**

After the connection is established but BEFORE constructing `BrainSession`, insert:

```rust
use spur_acp::session_lock::{SessionAttachGuard, AcquireOutcome};

let (attach_guard, fs_unsafe) =
    match SessionAttachGuard::try_acquire(&repo_root, &acp_session_id_str) {
        AcquireOutcome::Acquired(g) => (Some(g), false),
        AcquireOutcome::DegradedNoLock { reason } => {
            tracing::warn!(
                acp_id = %acp_session_id_str,
                reason = %reason,
                "flock unsupported on this volume; multi-instance protection disabled"
            );
            (None, true)
        }
        AcquireOutcome::Rejected { holder } => {
            return Err(LoadBrainSessionError::AlreadyAttached {
                acp_id: acp_session_id_str.clone(),
                holder,
            });
        }
        AcquireOutcome::Io(e) => {
            return Err(LoadBrainSessionError::Other(anyhow::Error::from(e)));
        }
    };
```

(`repo_root` must be in scope for the orchestrator; if not, plumb it through via `&self.repo_root`.)

- [ ] **Step 3: Thread `attach_guard` and `fs_unsafe` into the constructed `ActiveConnection`**

This requires extending `ActiveConnection` first — see Task 16.

- [ ] **Step 4: Catch the new error variant at the `load_brain_session` call site**

Search for `load_brain_session(` in `orchestrator.rs` (the resume path around lines 1473-1500). Where it currently emits `BrainError`, add a branch:

```rust
Err(LoadBrainSessionError::AlreadyAttached { acp_id, holder }) => {
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::SessionAttachRejected {
        acp_session_id: acp_id,
        holder,
        fs_unsafe: false,
    }));
}
```

### Task 15: Mirror the lock acquisition in `create_brain_session`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (`create_brain_session` function — find via `fn create_brain_session`)

- [ ] **Step 1: After the new ACP session id is returned, acquire the lock**

Same code as Task 14 Step 2, just inserted after the new `acp_session_id` is known.

- [ ] **Step 2: For a brand-new id, `Rejected` should be impossible**

If it happens, log a warning and proceed without the guard (defensive). Treat as `DegradedNoLock`. This is a sentinel for a programmer bug, not a user-facing error.

```rust
AcquireOutcome::Rejected { holder } => {
    tracing::error!(
        acp_id = %new_acp_id,
        ?holder,
        "newly-created session id is already locked — programmer bug"
    );
    (None, true)
}
```

### Task 16: Extend `ActiveConnection` with the lock fields

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (the struct from Task 1)

- [ ] **Step 1: Add fields**

```rust
pub struct ActiveConnection {
    pub transport: Box<dyn AgentConnection>,
    pub brain_name: String,
    /// `None` only when attached under DegradedNoLock (NFS/sshfs).
    /// Holding this for the lifetime of `transport` enforces the
    /// single-attach invariant.
    pub attach_guard: Option<spur_acp::session_lock::SessionAttachGuard>,
    /// True when this attachment is unprotected (multi-window unsafe).
    pub fs_unsafe: bool,
}
```

- [ ] **Step 2: Update every `ActiveConnection { ... }` constructor**

Search workspace:
```bash
rg -n 'ActiveConnection\s*\{' --type rust
```

For sites that reuse a connection without a new lock acquire (e.g., line 1342, 1444, 2366 from Phase 1a), preserve the previous guard:
- `retire_active_brain` (line 2366): the brain is being torn down but the connection persists. Move the guard from the retiring `BrainSession` (if it stored one) — OR keep the guard on `ActiveConnection` from the prior `load_brain_session` call. Confirm there is exactly ONE place that calls `try_acquire`, namely Task 14 + 15.

For all other sites, propagate `attach_guard: existing_guard, fs_unsafe: existing_fs_unsafe`.

- [ ] **Step 3: At the `load_brain_session` post-acquire site, store the guard**

Where `ActiveConnection` is built after a successful `load_brain_session`:
```rust
ActiveConnection {
    transport: conn,
    brain_name,
    attach_guard,    // from Task 14 Step 2
    fs_unsafe,
}
```

- [ ] **Step 4: Emit `AgentSessionReady` with the right `fs_unsafe`**

Where the orchestrator currently emits `AgentSessionReady`, pass `fs_unsafe: active_connection.fs_unsafe`.

- [ ] **Step 5: Run `cargo build --workspace`**

Expected: clean.

### Task 17: Cross-process integration test

**Files:**
- Create: `crates/spur-cli/tests/session_attach_collision.rs`

- [ ] **Step 1: Write the test**

```rust
//! Integration test: two `spur tui --session <id>` processes cannot
//! simultaneously attach to the same ACP session.

use assert_cmd::prelude::*;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
#[ignore = "requires built binary; run with `cargo test --release -- --ignored`"]
fn second_concurrent_session_attach_emits_rejected() {
    // This test assumes a previously-recorded session metadata fixture
    // exists at tests/fixtures/repo_with_session/.spur/session_metadata.json.
    // Set up: copy fixture into a temp dir, point both processes at it.
    let tmp = tempfile::TempDir::new().unwrap();
    spur_test_support::seed_repo_with_session(tmp.path(), "test-acp-id");

    let mut first = Command::cargo_bin("spur")
        .unwrap()
        .args(["tui", "--session", "test-acp-id"])
        .current_dir(tmp.path())
        .env("SPUR_HEADLESS_TEST", "1")  // suppress TUI rendering
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("first spawn");

    // Give the first process time to acquire the lock.
    std::thread::sleep(Duration::from_millis(500));

    let second = Command::cargo_bin("spur")
        .unwrap()
        .args(["tui", "--session", "test-acp-id"])
        .current_dir(tmp.path())
        .env("SPUR_HEADLESS_TEST", "1")
        .output()
        .expect("second spawn");

    // The second process should observe SessionAttachRejected in its
    // event log (headless test mode dumps events to stdout as JSON).
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("SessionAttachRejected"),
        "expected SessionAttachRejected in second process output, got: {stdout}"
    );

    // Cleanup
    first.kill().ok();
    first.wait().ok();
}
```

NOTE: this test depends on a `SPUR_HEADLESS_TEST` mode and a `spur_test_support` helper that may not exist. If they do not, mark this task as creating those scaffolds first — or skip the integration test for now and rely on the unit test from Task 7 which exercises the same code path within one process.

- [ ] **Step 2: Run with `cargo test --release -- --ignored`**

If the headless mode does not exist, leave this test `#[ignore]` and document the gap in the commit message.

### Task 18: Create `CollisionModal` component

**Files:**
- Create: `crates/spur-tui/src/components/collision_modal.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Create the component (mirrors `quit_confirm.rs`)**

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use spur_acp::session_lock::HolderInfo;

/// Modal shown when the user attempts to attach to a session that is
/// currently held by another `spur tui` process.
pub struct CollisionModal;

impl CollisionModal {
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        session_label: &str,
        holder: &HolderInfo,
    ) {
        let width = 70u16.min(area.width.saturating_sub(4));
        let height = 14u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Session attached in another window ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        // Holder identity: priority label > tty > pid+started_at; workdir always.
        let identity = if let Some(label) = holder.label.as_deref() {
            format!("  holder: {label}")
        } else if let Some(tty) = holder.tty.as_deref() {
            format!("  holder: {tty}")
        } else if let Some(pid) = holder.pid {
            let when = holder
                .started_at
                .map(|t| format!(" (started {})", t.format("%H:%M")))
                .unwrap_or_default();
            format!("  holder: PID {pid}{when}")
        } else {
            "  holder: another window (no metadata)".to_string()
        };

        let workdir_line = holder
            .workdir
            .as_ref()
            .map(|w| format!("  workdir: {}", w.display()))
            .unwrap_or_default();

        let kill_line = holder
            .pid
            .map(|pid| format!("    kill {pid}"))
            .unwrap_or_else(|| "    (no PID available — close the other window manually)".into());

        let lines = vec![
            Line::from(""),
            Line::from(format!("  {session_label}")),
            Line::from(Span::styled(identity, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(workdir_line, Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("[N]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" new session   "),
                Span::styled("[P]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(" picker   "),
                Span::styled("[Esc]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw(" cancel"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  To take over manually, run in your shell:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(kill_line, Style::default().fg(Color::White))),
            Line::from(Span::styled(
                "  then press [Enter] to retry attach.",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        frame.render_widget(Paragraph::new(lines).block(block), popup_area);
    }
}
```

- [ ] **Step 2: Register in components mod**

Add to `crates/spur-tui/src/components/mod.rs`:
```rust
pub mod collision_modal;
```

- [ ] **Step 3: Run `cargo check -p spur-tui`**

Expected: clean.

### Task 19: TUI handler — open `CollisionModal` on `SessionAttachRejected`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add modal state to `App`**

Find the `App` struct (or equivalent state holder). Add:

```rust
/// `Some` when the collision modal is open. Carries holder info.
collision_modal: Option<CollisionModalState>,
```

```rust
struct CollisionModalState {
    acp_id: String,
    holder: spur_acp::session_lock::HolderInfo,
}
```

- [ ] **Step 2: Handle the event**

Find the `SpurEventBody` event-handling `match` (search for other `SpurEventBody::` arms, e.g. `BrainError` or `AgentSessionReady`). Add:

```rust
SpurEventBody::SessionAttachRejected { acp_session_id, holder, fs_unsafe: _ } => {
    self.collision_modal = Some(CollisionModalState {
        acp_id: acp_session_id,
        holder,
    });
}
```

- [ ] **Step 3: Render the modal**

In the `App` render path, AFTER all views, add:

```rust
if let Some(state) = &self.collision_modal {
    CollisionModal::render(frame, area, &state.acp_id, &state.holder);
}
```

- [ ] **Step 4: Handle keys when modal is open**

In the key dispatch, add a guard at the top:

```rust
if self.collision_modal.is_some() {
    match key.code {
        KeyCode::Esc => self.collision_modal = None,
        KeyCode::Char('N') | KeyCode::Char('n') => {
            self.collision_modal = None;
            // Send NewSession intent (mirror the existing [n]ew action)
            // ... use existing UserInput::NewSessionWithMessage path
        }
        KeyCode::Char('P') | KeyCode::Char('p') => {
            self.collision_modal = None;
            // Open picker via existing path
            // ... navigate to SessionPickerView
        }
        KeyCode::Enter => {
            // Retry attach: re-send ResumeSession with the same acp_id
            let acp = self.collision_modal.as_ref().unwrap().acp_id.clone();
            self.collision_modal = None;
            self.send_user_input(UserInput::ResumeSession { session_id: acp });
        }
        _ => {}
    }
    return;  // swallow all other keys while modal is open
}
```

- [ ] **Step 5: Run `cargo build -p spur-tui`**

Expected: clean.

### Task 20: TUI handler — `fs_unsafe` banner in SessionDetail

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add `fs_unsafe` field to `SessionDetailView`**

Find the struct (around line 44 per the spec). Add:
```rust
fs_unsafe: bool,
```

- [ ] **Step 2: Set the field on `AgentSessionReady` event**

In the existing `SessionDetailView` event handler for `AgentSessionReady`, store the new field:
```rust
SpurEventBody::AgentSessionReady { fs_unsafe, .. } => {
    self.fs_unsafe = fs_unsafe;
    // ...existing logic...
}
```

- [ ] **Step 3: Render the banner when set**

In the render path, just above the input bar:
```rust
if self.fs_unsafe {
    let banner = Line::from(Span::styled(
        " ⚠ unsafe-fs: flock unsupported on this volume — multi-window protection OFF ",
        Style::default().fg(Color::Black).bg(Color::Yellow),
    ));
    let para = Paragraph::new(vec![banner]);
    // Render in a 1-row strip above the input bar.
    frame.render_widget(para, banner_area);
}
```

- [ ] **Step 4: Run `cargo build -p spur-tui`**

Expected: clean.

### Task 21: Commit Phase 1b

- [ ] **Step 1: Run all workspace tests**

Run: `cargo test --workspace --no-fail-fast`
Expected: green (or only the `#[ignore]`'d integration test from Task 17 missing, which is acceptable).

- [ ] **Step 2: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs \
        crates/spur-core/src/orchestrator.rs \
        crates/spur-tui/src/components/collision_modal.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-cli/tests/session_attach_collision.rs
git commit -m "feat: enforce single-attach invariant + collision modal (phase 1b)

SpurEventBody gains SessionAttachRejected variant and AgentSessionReady
gains fs_unsafe field. Orchestrator::load_brain_session and
create_brain_session acquire SessionAttachGuard via try_acquire and
store it on ActiveConnection so the lock lifetime tracks the transport.
TUI catches SessionAttachRejected and renders a centered CollisionModal
that surfaces the shell `kill <pid>` command as the escape hatch
(no --force-attach flag — SPUR never owns process-killing). NFS/sshfs
volumes attach with fs_unsafe=true and a persistent banner.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 2 — Picker Landing + `--session` Flag

### Task 22: Add `AttachExplicit` variant + `preselect` to `LandingDecision`

**Files:**
- Modify: `crates/spur-tui/src/landing.rs`

- [ ] **Step 1: Update the enum**

```rust
#[derive(Debug, Clone)]
pub enum LandingDecision {
    AutoResume { acp_id: String, brain: String },
    AttachExplicit { acp_id: String, brain: String },        // NEW
    ShowPicker { preselect: Option<String> },                // CHANGED: was unit
    ShowDashboard,
    SetupRequired,
}
```

- [ ] **Step 2: Run `cargo check --workspace`**

Expected: errors at `LandingDecision::ShowPicker` match arms (was unit, now needs field destructure). Update each:
```rust
LandingDecision::ShowPicker { preselect: _ } => { ... }
```

### Task 23: Add `preselect` field to `SessionPickerView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs:116`

- [ ] **Step 1: Add the field**

```rust
pub struct SessionPickerView {
    // ...existing fields...
    /// When `Some`, the picker preselects this acp_id on first render
    /// and shows a top banner. NEVER auto-fires Enter.
    preselect: Option<String>,
}
```

- [ ] **Step 2: Add a constructor that takes preselect**

```rust
impl SessionPickerView {
    pub fn with_preselect(preselect: Option<String>) -> Self {
        Self {
            preselect,
            ..Self::new()
        }
    }
}
```

- [ ] **Step 3: Run `cargo check -p spur-tui`**

Expected: clean.

### Task 24: Render top banner when `preselect` populated

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (the render method)

- [ ] **Step 1: In the render path, before the list, check preselect**

```rust
if let Some(ref acp_id) = self.preselect {
    let banner = self.build_preselect_banner(acp_id);
    // Render in a 1-row strip at the top of the picker chunk.
    frame.render_widget(Paragraph::new(banner), banner_area);
}
```

- [ ] **Step 2: Implement `build_preselect_banner`**

```rust
fn build_preselect_banner(&self, acp_id: &str) -> Line<'_> {
    // Find the matching session in self.state.
    let label = match &self.state {
        PickerState::Populated { sessions, .. } => sessions
            .iter()
            .find(|s| s.acp_session_id == *acp_id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| format!("(unknown) {}", &acp_id[..8.min(acp_id.len())])),
        _ => acp_id.to_string(),
    };
    Line::from(vec![
        Span::raw(" Last: "),
        Span::styled(label, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  ·  "),
        Span::styled("[Enter] resume", Style::default().fg(Color::Green)),
        Span::raw("  ·  "),
        Span::styled("[n] new", Style::default().fg(Color::DarkGray)),
    ])
}
```

- [ ] **Step 3: Run `cargo check -p spur-tui`**

Expected: clean.

### Task 25: Jump cursor to preselected row on first render

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (the place that transitions to `PickerState::Populated`)

- [ ] **Step 1: Find the populated-state assignment**

Search for `PickerState::Populated {` and the place where `cursor: 0` (default) is set after sessions arrive.

- [ ] **Step 2: Override the cursor based on preselect**

```rust
let cursor = self
    .preselect
    .as_ref()
    .and_then(|target| sessions.iter().position(|s| s.acp_session_id == *target))
    .unwrap_or(0);

self.state = PickerState::Populated {
    agent,
    sessions,
    cursor,
    search_focused: false,
    filter: String::new(),
};
```

- [ ] **Step 3: Run `cargo check -p spur-tui`**

Expected: clean.

### Task 26: Handle "preselect not found" sub-state

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (`build_preselect_banner` from Task 24)

- [ ] **Step 1: When the preselected id is not in the session list, render a different banner**

Update `build_preselect_banner` to detect this:
```rust
fn build_preselect_banner(&self, acp_id: &str) -> Line<'_> {
    if let PickerState::Populated { sessions, .. } = &self.state {
        if let Some(s) = sessions.iter().find(|s| s.acp_session_id == *acp_id) {
            // happy path — same as Task 24
        } else {
            return Line::from(vec![
                Span::styled(" ⚠ Session ", Style::default().fg(Color::Red)),
                Span::styled(&acp_id[..8.min(acp_id.len())], Style::default().fg(Color::Yellow)),
                Span::styled(" not found  ·  ", Style::default().fg(Color::Red)),
                Span::styled("[Enter] new", Style::default().fg(Color::Green)),
                Span::raw("  ·  "),
                Span::styled("[Esc] cancel", Style::default().fg(Color::DarkGray)),
            ]);
        }
    }
    Line::from(format!(" Loading session list for {acp_id}..."))
}
```

- [ ] **Step 2: Run `cargo check -p spur-tui`**

Expected: clean.

### Task 27: Add `--session` CLI flag

**Files:**
- Modify: `crates/spur-cli/src/main.rs:220-239`

- [ ] **Step 1: Add the flag to the `Tui` variant**

```rust
Tui {
    #[arg(long)]
    brain: Option<String>,
    #[arg(long)]
    sessions: bool,
    #[arg(long)]
    dashboard: bool,
    #[arg(long)]
    new: bool,
    /// Attach to a specific ACP session by id. Auto-fires the resume
    /// after launching the picker, with the session preselected.
    #[arg(long)]
    session: Option<String>,                                 // NEW
    #[arg(long)]
    profile: bool,
    #[arg(long, default_value = "30")]
    duration: u64,
},
```

- [ ] **Step 2: Run `cargo check -p spur-cli`**

Expected: clean.

### Task 28: Update `resolve_landing()` precedence

**Files:**
- Modify: `crates/spur-cli/src/main.rs:56`

- [ ] **Step 1: Update signature to take `session: Option<&str>`**

```rust
fn resolve_landing(
    new: bool,
    sessions: bool,
    dashboard: bool,
    session: Option<&str>,                                   // NEW
    brain_override: Option<&str>,
    meta: &spur_tui::session_metadata::SessionMetadataStore,
    registry: &spur_acp::AgentRegistry,
) -> spur_tui::landing::LandingDecision {
```

- [ ] **Step 2: Update the body precedence**

```rust
use spur_tui::landing::LandingDecision;
if new {
    return LandingDecision::ShowDashboard;
}
if let Some(acp) = session {
    let brain = brain_override
        .map(str::to_string)
        .or_else(|| meta.brain_for_acp(acp))
        .unwrap_or_else(|| "claude-code".to_string());
    return LandingDecision::AttachExplicit {
        acp_id: acp.to_string(),
        brain,
    };
}
if sessions && !dashboard {
    return LandingDecision::ShowPicker { preselect: None };
}
// ...rest unchanged, but ShowPicker must include `{ preselect: None }`...

if let Some((acp, stored_brain)) = meta.last_active_acp() {
    if /* brain_matches && fresh */ {
        return LandingDecision::AutoResume { acp_id: acp, brain: stored_brain };
    }
}

if meta.has_any_session() {
    return LandingDecision::ShowPicker { preselect: None };
}
LandingDecision::ShowDashboard
```

- [ ] **Step 3: Update the call site (line 671)**

```rust
let landing = resolve_landing(
    new,
    sessions,
    dashboard,
    session.as_deref(),                                      // NEW
    brain_for_resume.as_deref(),
    &meta,
    &orch.registry,
);
```

(Add a `brain_for_acp` helper to `SessionMetadataStore` if it doesn't exist; it returns the stored brain for a given acp_id.)

- [ ] **Step 4: Run `cargo check -p spur-cli`**

Expected: clean.

### Task 29: Update CLI dispatch (lines 717-738) for AttachExplicit and AutoResume

**Files:**
- Modify: `crates/spur-cli/src/main.rs:714-738`

- [ ] **Step 1: Replace the `match &landing` block**

```rust
use spur_tui::landing::LandingDecision;
let force_picker_with_preselect: Option<Option<String>> = match &landing {
    LandingDecision::AutoResume { acp_id, .. } => Some(Some(acp_id.clone())),
    LandingDecision::AttachExplicit { acp_id, .. } => Some(Some(acp_id.clone())),
    LandingDecision::ShowPicker { preselect } => Some(preselect.clone()),
    _ => None,
};

match &landing {
    LandingDecision::AttachExplicit { acp_id, .. } => {
        // Mirrors the existing AutoResume dispatch: send ResumeSession at startup.
        let resume_tx = tui_tx.clone();
        let id = acp_id.clone();
        tokio::spawn(async move {
            let _ = resume_tx
                .send(spur_tui::UserInput::ResumeSession { session_id: id })
                .await;
        });
    }
    LandingDecision::AutoResume { .. } => {
        // NO automatic ResumeSession. The picker preselects; user must press Enter.
    }
    LandingDecision::ShowPicker { .. } => {
        // Picker opens; no preload.
    }
    LandingDecision::ShowDashboard | LandingDecision::SetupRequired => {
        let warm_handle = host.handle();
        tokio::spawn(async move {
            let _ = warm_handle
                .send_command(spur_core::InteractiveInput::WarmConnect)
                .await;
        });
    }
}
```

- [ ] **Step 2: Plumb `preselect` into `run_tui_with_license`**

The function signature at line 743 currently takes `force_picker: bool`. Replace with `start_in_picker_with_preselect: Option<Option<String>>` (or similar). The TUI then constructs `SessionPickerView::with_preselect(preselect)` instead of `SessionPickerView::new()` when this is `Some`.

If preserving the old `force_picker` boolean is preferred for compatibility, pass a separate `preselect: Option<String>` argument and have `run_tui_with_license` handle both.

- [ ] **Step 3: Run `cargo build --workspace`**

Expected: clean.

### Task 30: Add unit test for `resolve_landing` precedence

**Files:**
- Modify: `crates/spur-cli/src/main.rs` (existing tests module, or create one)

- [ ] **Step 1: Write the test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn empty_store() -> spur_tui::session_metadata::SessionMetadataStore {
        spur_tui::session_metadata::SessionMetadataStore::default()
    }
    fn populated_registry() -> spur_acp::AgentRegistry {
        // build a minimal registry with one agent — adapt to the real ctor
        unimplemented!("use the existing test helper if available; otherwise stub")
    }

    #[test]
    fn explicit_session_returns_attach_explicit() {
        let landing = resolve_landing(
            false, false, false,
            Some("abc-123"),
            None,
            &empty_store(),
            &populated_registry(),
        );
        assert!(matches!(
            landing,
            spur_tui::landing::LandingDecision::AttachExplicit { acp_id, .. } if acp_id == "abc-123"
        ));
    }

    #[test]
    fn new_flag_overrides_session_flag() {
        let landing = resolve_landing(
            /*new*/ true, false, false,
            Some("abc-123"),
            None,
            &empty_store(),
            &populated_registry(),
        );
        assert!(matches!(landing, spur_tui::landing::LandingDecision::ShowDashboard));
    }
}
```

If the existing test infrastructure makes this awkward, write the equivalent assertions inline in an integration test under `crates/spur-cli/tests/`.

- [ ] **Step 2: Run the tests**

Run: `cargo test -p spur-cli resolve_landing`
Expected: PASS.

### Task 31: Manual UX verification + commit Phase 2

- [ ] **Step 1: Build the binary**

Run: `cargo build --release --bin spur`
Expected: clean.

- [ ] **Step 2: Manual smoke test in a real terminal**

Open a terminal and run inside a SPUR-initialized repo with at least one prior session:

1. `spur tui` → should land on `SessionPickerView` with the last session preselected (cursor on top row, banner reads `Last: <name> · ...`)
2. Press `Enter` → should attach + open `SessionDetail`
3. Open a SECOND terminal, run `spur tui --session <same-id>` → should open picker preselected, auto-attempt attach, **CollisionModal** should appear with holder info
4. In the modal, press `[Esc]` → modal closes; user is on picker
5. Close the FIRST terminal; back in second, press `[Enter]` on the row → should now attach successfully
6. (NFS test, if available) Mount a sshfs volume, run `spur tui` there → should attach with `⚠ unsafe-fs` banner

- [ ] **Step 3: Commit Phase 2**

```bash
git add crates/spur-tui/src/landing.rs \
        crates/spur-tui/src/views/session_picker.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-cli/src/main.rs
git commit -m "feat: --session flag + picker landing with preselect (phase 2)

LandingDecision gains AttachExplicit variant; ShowPicker carries
preselect: Option<String>. spur tui --session <id> opens the picker
preselected and dispatches ResumeSession on launch (auto-attach because
the flag IS the explicit consent). Bare spur tui's AutoResume path is
rerouted to the same picker-with-preselect flow, but does NOT
auto-dispatch — user must press Enter (the axiom: no implicit attach).
SessionPickerView gains a top banner and cursor-jump-on-first-render.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 3 — Polish (Out of scope for this plan)

These belong in a follow-up plan once Phase 1+2 ship and bake:

- `fs_unsafe` banner auto-collapse to header tag after first keystroke
- Centered onboarding state for zero-sessions launch
- `SPUR_TUI_LABEL` env var → propagated into `HolderInfo.label` (already partially in `try_acquire` via `std::env::var`)
- Render-time `⚠ attached:<pid>` row badges in picker (cheap polling via `Tick`, 2s cadence)
- TTY detection: `nix::unistd::ttyname(libc::STDIN_FILENO)` on Unix; `None` on Windows

---

## Self-Review

**Spec coverage:**
- §1 problem → addressed by Phases 1a+1b (lock + invariant enforcement)
- §2 invariant → enforced by `attach_guard` lifetime tied to `transport` in `ActiveConnection`
- §3 acceptance criteria C1–C7 → C1 (Task 17), C2 (Task 31 manual), C3 (Task 18 modal copy), C4 (Task 8 cross-platform classifier), C5 (Tasks 6/8 ENOTSUP path), C6 (Task 3 + 21 test runs), C7 (Phase 1b ships independently per Task 21 commit)
- §4 architecture → §4.1 (Tasks 5–11), §4.2 (Tasks 14–16), §4.3 (Tasks 12–13), §4.4 (Task 27), §4.5 (Tasks 22, 28, 29)
- §5 picker changes → Tasks 23–26
- §6 wireframes → §6.1 (Tasks 23–25), §6.2 (Task 18), §6.3 (Task 20), §6.4 deferred to Phase 3
- §7 phasing → followed: Phase 1a = Tasks 1–3, Phase 1b = Tasks 4–21, Phase 2 = Tasks 22–31
- §8 test plan → unit tests in Tasks 6–10, integration in Task 17, manual in Task 31; CI matrix is part of CI config (out of scope for this plan)
- §9 migration → no migration tasks needed (lockfiles are created on first attach; metadata schema unchanged)

**Placeholder scan:** Task 17 contains a `#[ignore]`'d test that depends on a `SPUR_HEADLESS_TEST` mode that may not exist — explicitly flagged with a fallback (rely on Task 7 unit test). Task 30 has `unimplemented!("use the existing test helper if available; otherwise stub")` — flagged inline as a known gap requiring discovery during implementation.

**Type consistency:** `ActiveConnection` field names (`transport`, `brain_name`, `attach_guard`, `fs_unsafe`) are used consistently across Tasks 1, 2, 16. `HolderInfo` field names match across `session_lock.rs` (Task 5), event variant (Task 12), and modal (Task 18). `LandingDecision::ShowPicker { preselect: Option<String> }` is used consistently across Tasks 22, 28, 29.

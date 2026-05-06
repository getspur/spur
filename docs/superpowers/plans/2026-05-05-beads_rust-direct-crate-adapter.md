# beads_rust Direct Crate Adapter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace SPUR's `Command::new("br")` shellout with direct linkage of the `beads_rust` 0.2.1 crate, including a multi-instance-safe adapter (4 primitives + init guards + observability) and a test-fixture helper that lets tests use the crate without invoking `br` at all.

**Architecture:** New `BeadsCrateAdapter` lives in `spur-pm/src/beads_crate.rs`, implements the existing `IssueTracker` trait. Reader pool + writer Mutex + `tokio::task::spawn_blocking` for the sync→async bridge. Cross-process safety via `beads_rust`'s `.beads/.write.lock` flock; intra-process Connection-handle safety via the writer Mutex. Snapshot CAS pair (`read_snapshot` + `validate_and_commit`) replaces optimistic row-level CAS (which the crate's public API does not support). Pre-launch context: clean replacement, no feature flag, no shadow validation.

**Tech Stack:** Rust 2024, Tokio (async runtime), `beads_rust` 0.2.1 (issue tracker crate), `rusqlite` (transitive), `fs2` (already in spur-pm for file locks), existing tracing/metrics layer.

**Spec:** `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md`

**Companion deferred items (NOT in this plan):** bd-1h3w reader-resilience, periodic JSONL integrity check, manual repair docs.

---

## File structure

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-pm/Cargo.toml` | Modify | Add `beads_rust = "0.2.1"` dependency |
| `crates/spur-pm/src/beads_crate.rs` | Create | New direct-crate adapter (the central artifact) |
| `crates/spur-pm/src/beads_crate/metrics.rs` | Create | `ContentionMetrics` struct + tracing hooks |
| `crates/spur-pm/src/beads_crate/backoff.rs` | Create | `BackoffPolicy` for transient flock errors |
| `crates/spur-pm/src/beads_crate/reader_pool.rs` | Create | Small fixed pool of `SqliteStorage` reader connections |
| `crates/spur-pm/src/beads_crate/init.rs` | Create | Init-time guards: FS detection, stale-tmp sweep, stale-JSONL detection, migration-under-lock |
| `crates/spur-pm/src/beads_crate/snapshot.rs` | Create | Snapshot CAS types (`Snapshot<S>`, `Conflict` error) |
| `crates/spur-pm/src/lib.rs` | Modify | `pub mod beads_crate;`; remove `pub mod beads;` |
| `crates/spur-pm/src/beads.rs` | Delete | Old shellout adapter — gone |
| `crates/spur-pm/src/service.rs` | Modify | `PmService::try_new` constructs `BeadsCrateAdapter` instead of `BeadsAdapter` |
| `crates/spur-pm/src/test_workspace.rs` | Create | `TestBeadsWorkspace` helper that uses `beads_rust` directly (no `br` CLI) |
| `crates/spur-pm/tests/beads_crate_primitives.rs` | Create | Unit tests for each primitive |
| `crates/spur-pm/tests/beads_crate_multiprocess.rs` | Create | Multi-process integration tests |
| `crates/spur-mcp/src/server.rs` | Modify | Test fixtures (lines 7483, 9039, 9205, 9270) use `TestBeadsWorkspace` |
| `crates/spur-mcp/src/plan/reconciler.rs` | Modify | Test fixtures (lines 2206-2411) use `TestBeadsWorkspace` |
| `crates/spur-mcp/tests/*.rs` | Modify | All test files using `Command::new("br")` switched to `TestBeadsWorkspace` (batch task) |
| `crates/spur-mcp/tests/common/g_strict_harness.rs` | Modify | Strict-harness setup uses `TestBeadsWorkspace` |
| `crates/spur-pm/tests/*.rs` | Modify | All `Command::new("br")` callers switched (batch task) |
| `crates/spur-cli/src/commands/init.rs` | Verify | If it shells to `br init`, replace with direct crate call |

---

## Section A — Foundation (Tasks 1-5)

### Task 1: Add `beads_rust` dependency

**Files:**
- Modify: `crates/spur-pm/Cargo.toml`

- [ ] **Step 1: Add dependency**

```toml
# In [dependencies] section of crates/spur-pm/Cargo.toml, add:
beads_rust = { git = "https://github.com/Dicklesworthstone/beads_rust", tag = "v0.2.1", default-features = false }
semver = "1"
```

- [ ] **Step 2: Verify it builds**

Run: `cd crates/spur-pm && cargo check`
Expected: clean build (warnings about unused `beads_rust` are OK; we haven't used it yet).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/Cargo.toml Cargo.lock
git commit -m "spur-pm: add beads_rust 0.2.1 as direct crate dep"
```

---

### Task 2: Skeleton module + ContentionMetrics

**Files:**
- Create: `crates/spur-pm/src/beads_crate/mod.rs`
- Create: `crates/spur-pm/src/beads_crate/metrics.rs`
- Modify: `crates/spur-pm/src/lib.rs`

- [ ] **Step 1: Write metrics tests first**

Create `crates/spur-pm/src/beads_crate/metrics.rs`:

```rust
//! Contention and operation metrics for the beads crate adapter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct ContentionMetrics {
    pub lock_wait_total_us: AtomicU64,
    pub lock_busy_total: AtomicU64,
    pub lock_ceiling_total: AtomicU64,
    pub write_total: AtomicU64,
    pub write_error_total: AtomicU64,
    pub read_total: AtomicU64,
    pub conflict_total: AtomicU64,
    pub conflict_exhausted_total: AtomicU64,
    pub auto_flush_skipped_total: AtomicU64,
    pub auto_flush_dirty_total: AtomicU64,
    pub auto_flush_success_total: AtomicU64,
    pub tmp_sweep_removed_total: AtomicU64,
}

impl ContentionMetrics {
    pub fn record_lock_wait(&self, d: Duration) {
        self.lock_wait_total_us.fetch_add(d.as_micros() as u64, Ordering::Relaxed);
    }
    pub fn incr_busy(&self) { self.lock_busy_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_ceiling(&self) { self.lock_ceiling_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_write(&self) { self.write_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_write_error(&self) { self.write_error_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_read(&self) { self.read_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_conflict(&self) { self.conflict_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_conflict_exhausted(&self) { self.conflict_exhausted_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_auto_flush_skipped(&self) { self.auto_flush_skipped_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_auto_flush_dirty(&self) { self.auto_flush_dirty_total.fetch_add(1, Ordering::Relaxed); }
    pub fn incr_auto_flush_success(&self) { self.auto_flush_success_total.fetch_add(1, Ordering::Relaxed); }
    pub fn add_tmp_sweep_removed(&self, n: u64) { self.tmp_sweep_removed_total.fetch_add(n, Ordering::Relaxed); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_increment() {
        let m = ContentionMetrics::default();
        m.incr_write();
        m.incr_write();
        m.incr_read();
        assert_eq!(m.write_total.load(Ordering::Relaxed), 2);
        assert_eq!(m.read_total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn lock_wait_accumulates_microseconds() {
        let m = ContentionMetrics::default();
        m.record_lock_wait(Duration::from_millis(5));
        m.record_lock_wait(Duration::from_millis(3));
        assert_eq!(m.lock_wait_total_us.load(Ordering::Relaxed), 8_000);
    }
}
```

- [ ] **Step 2: Create the parent module**

Create `crates/spur-pm/src/beads_crate/mod.rs`:

```rust
//! Direct-linkage adapter to the `beads_rust` 0.2.1 crate.
//!
//! See `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md`
//! for the full design.

pub mod metrics;
pub mod backoff;
pub mod reader_pool;
pub mod init;
pub mod snapshot;
```

- [ ] **Step 3: Wire into lib.rs**

In `crates/spur-pm/src/lib.rs`, add:

```rust
pub mod beads_crate;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-pm beads_crate::metrics --lib`
Expected: 2 tests pass. (`backoff`, `reader_pool`, `init`, `snapshot` modules don't exist yet — compile errors expected for those mod entries; defer wiring until later tasks.)

- [ ] **Step 5: Comment out the not-yet-existing modules**

Adjust `crates/spur-pm/src/beads_crate/mod.rs` to only export what exists:

```rust
pub mod metrics;
// pub mod backoff;       // Task 3
// pub mod reader_pool;   // Task 4
// pub mod init;          // Task 5/6/7/8
// pub mod snapshot;      // Task 11
```

- [ ] **Step 6: Re-run tests, then commit**

Run: `cargo test -p spur-pm beads_crate::metrics --lib`
Expected: 2 pass.

```bash
git add crates/spur-pm/src/beads_crate/ crates/spur-pm/src/lib.rs
git commit -m "spur-pm: scaffold beads_crate module and ContentionMetrics"
```

---

### Task 3: BackoffPolicy

**Files:**
- Create: `crates/spur-pm/src/beads_crate/backoff.rs`

- [ ] **Step 1: Write tests first**

Create `crates/spur-pm/src/beads_crate/backoff.rs`:

```rust
//! Exponential-backoff-with-jitter retry policy for transient flock contention.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub max_step: Duration,
    pub factor: f64,
    pub jitter: f64,
    pub ceiling: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(50),
            max_step: Duration::from_secs(2),
            factor: 1.5,
            jitter: 0.25,
            ceiling: Duration::from_secs(10),
        }
    }
}

impl BackoffPolicy {
    /// Compute the nth step delay given a deterministic random source.
    /// Returns None if `elapsed >= ceiling` (caller should give up).
    pub fn step(&self, attempt: u32, elapsed: Duration, rand_unit: f64) -> Option<Duration> {
        if elapsed >= self.ceiling { return None; }
        let base_ms = self.initial.as_secs_f64() * 1000.0 * self.factor.powi(attempt as i32);
        let capped_ms = base_ms.min(self.max_step.as_secs_f64() * 1000.0);
        // jitter: rand_unit in [0,1] → multiplier in [1-jitter, 1+jitter]
        let jitter_mult = 1.0 - self.jitter + (2.0 * self.jitter * rand_unit);
        let step_ms = (capped_ms * jitter_mult).max(0.0);
        Some(Duration::from_millis(step_ms as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_step_is_near_initial() {
        let p = BackoffPolicy::default();
        let d = p.step(0, Duration::ZERO, 0.5).unwrap();
        // initial=50ms, jitter=0.25 → range [37.5, 62.5]ms; with rand=0.5 → 50ms
        assert!(d >= Duration::from_millis(37) && d <= Duration::from_millis(63), "{:?}", d);
    }

    #[test]
    fn caps_at_max_step() {
        let p = BackoffPolicy::default();
        // attempt=20: 50ms * 1.5^20 = ~3.3s, but max_step=2s
        let d = p.step(20, Duration::ZERO, 0.5).unwrap();
        // jitter range around 2000ms: [1500, 2500]
        assert!(d <= Duration::from_millis(2500));
    }

    #[test]
    fn returns_none_past_ceiling() {
        let p = BackoffPolicy::default();
        assert!(p.step(0, Duration::from_secs(11), 0.5).is_none());
    }

    #[test]
    fn jitter_extremes_bounded() {
        let p = BackoffPolicy::default();
        let lo = p.step(0, Duration::ZERO, 0.0).unwrap();
        let hi = p.step(0, Duration::ZERO, 1.0).unwrap();
        assert!(lo < hi);
        assert!(lo >= Duration::from_millis(37));
        assert!(hi <= Duration::from_millis(63));
    }
}
```

- [ ] **Step 2: Enable module in mod.rs**

In `crates/spur-pm/src/beads_crate/mod.rs`, uncomment:

```rust
pub mod backoff;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm beads_crate::backoff --lib`
Expected: 4 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: add BackoffPolicy for flock contention retry"
```

---

### Task 4: ReaderPool

**Files:**
- Create: `crates/spur-pm/src/beads_crate/reader_pool.rs`

> **Plan revision (2026-05-05):** the original code in this task was
> defective — `loop { … return … }` with `free.capacity() < self.capacity`
> never actually bounded concurrency, causing unbounded `SqliteStorage::open`
> calls under contention (see bd-xb1t). The corrected version below uses a
> `tokio::sync::Semaphore` to enforce capacity. Also note that `beads_rust
> 0.2.1`'s underlying fsqlite engine is `!Send`, so the pool must be used on
> a single thread (current_thread runtime, LocalSet, or a dedicated worker
> thread). Tests reflect that constraint by using `pin!` + `timeout` instead
> of `tokio::spawn`.

- [ ] **Step 1: Write the file**

Create `crates/spur-pm/src/beads_crate/reader_pool.rs`:

```rust
//! Small fixed pool of `beads_rust::SqliteStorage` reader connections.
//!
//! WAL mode permits concurrent readers via *multiple* connections. One
//! connection cannot be called concurrently. The pool gives us bounded
//! concurrency without unbounded handle growth.
//!
//! Capacity is enforced by an async semaphore: at most `capacity` permits
//! are outstanding at any time, so at most `capacity` connections exist.
//! Connections are opened lazily on first miss and reused thereafter.
//!
//! ## Threading note
//!
//! `beads_rust 0.2.1`'s underlying SQLite engine (fsqlite) is `!Send` — it
//! holds `Rc<RefCell<…>>` internally. Consequences:
//!   - A `ReaderGuard` (and the `SqliteStorage` it wraps) cannot move across
//!     thread boundaries.
//!   - `ReaderPool` itself is `!Send`, so it cannot be cloned into a
//!     `tokio::spawn`'d task on a multi-threaded runtime.
//!   - All pool usage must stay on a single thread (e.g., `current_thread`
//!     runtime, `LocalSet` / `spawn_local`, or a dedicated worker thread that
//!     owns the pool).

use std::path::PathBuf;
use std::sync::Mutex;

use beads_rust::storage::sqlite::SqliteStorage;
use tokio::sync::{Semaphore, SemaphorePermit};

pub struct ReaderPool {
    beads_dir: PathBuf,
    free: Mutex<Vec<SqliteStorage>>,
    capacity: Semaphore,
}

impl ReaderPool {
    pub fn new(beads_dir: PathBuf, capacity: usize) -> Self {
        assert!(capacity > 0, "ReaderPool capacity must be > 0");
        Self {
            beads_dir,
            free: Mutex::new(Vec::with_capacity(capacity)),
            capacity: Semaphore::new(capacity),
        }
    }

    /// Check out a reader connection. Awaits a capacity permit, then either
    /// reuses a free connection or opens a new one. The returned guard holds
    /// the permit; dropping it returns the connection and releases capacity.
    pub async fn checkout(&self) -> anyhow::Result<ReaderGuard<'_>> {
        let permit = self
            .capacity
            .acquire()
            .await
            .expect("ReaderPool semaphore never closed");
        let cached = {
            let mut free = self.free.lock().unwrap();
            free.pop()
        };
        let conn = match cached {
            Some(c) => c,
            None => SqliteStorage::open(&self.beads_dir.join("beads.db"))?,
        };
        Ok(ReaderGuard { pool: self, conn: Some(conn), _permit: permit })
    }
}

pub struct ReaderGuard<'a> {
    pool: &'a ReaderPool,
    conn: Option<SqliteStorage>,
    _permit: SemaphorePermit<'a>,
}

impl<'a> ReaderGuard<'a> {
    pub fn storage(&self) -> &SqliteStorage {
        self.conn.as_ref().expect("guard always has conn before drop")
    }
}

impl<'a> Drop for ReaderGuard<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut free = self.pool.free.lock().unwrap();
            free.push(conn);
        }
        // _permit drop releases the semaphore slot.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn init_beads(dir: &std::path::Path) {
        let _ = SqliteStorage::open(&dir.join("beads.db")).expect("open initializes schema");
    }

    #[tokio::test]
    async fn checkout_returns_a_reader() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let pool = ReaderPool::new(dir.path().to_path_buf(), 2);
        let r = pool.checkout().await.expect("checkout");
        let _ = r.storage();
    }

    #[tokio::test]
    async fn drop_returns_to_pool() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let pool = ReaderPool::new(dir.path().to_path_buf(), 1);
        {
            let _ = pool.checkout().await.unwrap();
        }
        assert_eq!(pool.free.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn checkout_blocks_when_at_capacity() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let pool = ReaderPool::new(dir.path().to_path_buf(), 1);

        let g1 = pool.checkout().await.expect("first checkout");

        let second = pool.checkout();
        tokio::pin!(second);

        let blocked = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
        assert!(blocked.is_err(), "second checkout completed while permit was held");

        drop(g1);

        let g2 = tokio::time::timeout(Duration::from_millis(500), &mut second)
            .await
            .expect("second checkout did not unblock after first was dropped")
            .expect("checkout returned an error");
        let _ = g2.storage();
    }

    #[tokio::test]
    async fn capacity_caps_open_connections() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let capacity = 3usize;
        let pool = ReaderPool::new(dir.path().to_path_buf(), capacity);

        let mut guards = Vec::new();
        for _ in 0..capacity {
            guards.push(pool.checkout().await.expect("checkout"));
        }

        let waiter = pool.checkout();
        tokio::pin!(waiter);

        let blocked = tokio::time::timeout(Duration::from_millis(50), &mut waiter).await;
        assert!(blocked.is_err(), "(N+1)th checkout did not block");

        drop(guards.pop());

        let g = tokio::time::timeout(Duration::from_millis(500), &mut waiter)
            .await
            .expect("(N+1)th checkout did not unblock")
            .expect("checkout returned an error");
        let _ = g.storage();

        drop(guards);
        drop(g);
        assert!(pool.free.lock().unwrap().len() <= capacity);
    }
}
```

- [ ] **Step 2: Enable module**

In `crates/spur-pm/src/beads_crate/mod.rs`, uncomment:

```rust
pub mod reader_pool;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm beads_crate::reader_pool --lib`
Expected: 4 pass (2 happy-path + 2 contention).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: add semaphore-bounded ReaderPool for WAL readers"
```

---

### Task 5: Snapshot CAS types

**Files:**
- Create: `crates/spur-pm/src/beads_crate/snapshot.rs`

- [ ] **Step 1: Write the file**

```rust
//! Snapshot CAS pattern types. See spec section "Snapshot re-validation pattern".

use std::fmt;

/// A snapshot captured by `read_snapshot`, used as a CAS token by
/// `validate_and_commit`.
#[derive(Debug, Clone)]
pub struct Snapshot<S> {
    pub value: S,
    /// SQLite `PRAGMA data_version` at read time. Cheap monotonic counter
    /// that bumps whenever any other connection commits a write.
    pub data_version: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("snapshot CAS conflict: state changed between read and validate")]
pub struct Conflict {
    pub data_version_expected: i64,
    pub data_version_actual: i64,
    pub detail: Option<String>,
}

impl Conflict {
    pub fn data_version(expected: i64, actual: i64) -> Self {
        Self { data_version_expected: expected, data_version_actual: actual, detail: None }
    }

    pub fn with_detail(mut self, msg: impl fmt::Display) -> Self {
        self.detail = Some(msg.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_carries_value_and_version() {
        let s = Snapshot { value: 42_u32, data_version: 7 };
        assert_eq!(s.value, 42);
        assert_eq!(s.data_version, 7);
    }

    #[test]
    fn conflict_default_no_detail() {
        let c = Conflict::data_version(3, 5);
        assert!(c.detail.is_none());
        assert_eq!(c.data_version_expected, 3);
        assert_eq!(c.data_version_actual, 5);
    }

    #[test]
    fn conflict_with_detail() {
        let c = Conflict::data_version(3, 5).with_detail("issue bd-x changed");
        assert_eq!(c.detail.as_deref(), Some("issue bd-x changed"));
    }
}
```

- [ ] **Step 2: Enable module + run tests + commit**

In `crates/spur-pm/src/beads_crate/mod.rs`, uncomment `pub mod snapshot;`.

Run: `cargo test -p spur-pm beads_crate::snapshot --lib`
Expected: 3 pass.

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: add Snapshot CAS types (Snapshot, Conflict)"
```

---

## Section B — Init guards (Tasks 6-9)

### Task 6: Filesystem detection

> **Plan revision (2026-05-05):** bd-f2mp fixed two corner cases in the
> Task 6 snippet: paths that cannot be C-stringified now best-effort allow
> instead of panicking, and Linux filesystem magic comparisons cast `f_type`
> through `u32` first (`as u32 as u64`) so CIFS detection works on 32-bit
> Linux where `f_type` is `i32` and a direct `as u64` would sign-extend the
> high bit. Test cfg tightened to the OSes whose code paths the test
> exercises, and a `cifs_magic_does_not_sign_extend_through_i32` regression
> test locks the cast contract regardless of build target.

**Files:**
- Create: `crates/spur-pm/src/beads_crate/init.rs`

- [ ] **Step 1: Write the file (FS detection only — other guards in next tasks)**

```rust
//! Init-time guards for the beads crate adapter.
//!
//! Each function is a precondition that must hold before `BeadsCrateAdapter`
//! is allowed to open the writer connection.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("non-local filesystem detected at {path}: fs_type = {fs_type}. \
             flock semantics are not portable here. \
             Set allow_non_local_fs=true in config to bypass.")]
    NonLocalFilesystem { path: String, fs_type: String },
}

/// Returns Ok(()) for local filesystems; Err for known network mounts (NFS, SMB, etc.).
/// Best-effort: on platforms where we cannot determine the FS type, returns Ok.
pub fn detect_local_fs(beads_dir: &Path) -> Result<(), InitError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = CString::new(beads_dir.as_os_str().as_bytes()) else {
            return Ok(()); // path can't be C-stringified — best-effort allow
        };
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 { return Ok(()); } // can't determine; allow
        // Magic numbers from <linux/magic.h>; kernel stores them as __u32.
        // On 32-bit Linux, libc::statfs.f_type is i32; per the Rust reference,
        // `i32 as u64` SIGN-extends. Cast through u32 first to zero-extend so
        // CIFS (high bit set) compares correctly.
        const NFS_SUPER_MAGIC: u64 = 0x6969;
        const SMB_SUPER_MAGIC: u64 = 0x517B;
        const CIFS_MAGIC_NUMBER: u64 = 0xFF534D42;
        let ty = buf.f_type as u32 as u64;
        if ty == NFS_SUPER_MAGIC || ty == SMB_SUPER_MAGIC || ty == CIFS_MAGIC_NUMBER {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: format!("0x{:x}", ty),
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = CString::new(beads_dir.as_os_str().as_bytes()) else {
            return Ok(()); // path can't be C-stringified — best-effort allow
        };
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 { return Ok(()); }
        let fs_name = unsafe {
            let raw = buf.f_fstypename.as_ptr();
            std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
        };
        if matches!(fs_name.as_str(), "nfs" | "smbfs" | "cifs" | "afpfs") {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: fs_name,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn local_tempdir_is_local() {
        let dir = TempDir::new().unwrap();
        // tmpfs / APFS local — should pass
        assert!(detect_local_fs(dir.path()).is_ok());
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn nul_byte_in_path_does_not_panic() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;
        // Construct a path with an interior NUL byte. CString::new will reject this.
        let bad = PathBuf::from(OsString::from_vec(vec![b'a', 0, b'b']));
        // Best-effort: must not panic. Result is `Ok(())` because we can't determine FS.
        assert!(detect_local_fs(&bad).is_ok());
    }

    /// Locks the cast contract used in the Linux branch: i32 sign-extends
    /// through `as u64`, but `as u32 as u64` zero-extends. CIFS magic has the
    /// high bit set, so the choice matters on 32-bit Linux.
    #[test]
    fn cifs_magic_does_not_sign_extend_through_i32() {
        let signed: i32 = 0xFF534D42_u32 as i32;
        assert!(signed.is_negative());
        let direct: u64 = signed as u64;
        let via_u32: u64 = signed as u32 as u64;
        assert_eq!(direct, 0xFFFFFFFF_FF534D42_u64);
        assert_eq!(via_u32, 0xFF534D42_u64);
        assert_ne!(direct, via_u32);
    }
}
```

- [ ] **Step 2: Add libc to Cargo.toml**

Add to `[dependencies]` in `crates/spur-pm/Cargo.toml`:

```toml
libc = "0.2"
```

- [ ] **Step 3: Enable module**

In `crates/spur-pm/src/beads_crate/mod.rs`, uncomment:

```rust
pub mod init;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-pm beads_crate::init --lib`
Expected: 3 pass for Task 6 (`local_tempdir_is_local`,
`nul_byte_in_path_does_not_panic`, `cifs_magic_does_not_sign_extend_through_i32`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/Cargo.toml Cargo.lock crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: add detect_local_fs init guard"
```

---

### Task 7: Stale-tmp sweep

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/init.rs`

- [ ] **Step 1: Add the sweep function**

Append to `crates/spur-pm/src/beads_crate/init.rs`:

```rust
use std::time::{Duration, SystemTime};

/// Pattern matching the temp files beads_rust creates during atomic JSONL writes.
/// Per beads_rust 0.2.1 `sync::export_temp_path`:
///
/// ```ignore
/// pub(crate) fn export_temp_path(output_path: &Path) -> PathBuf {
///     output_path.with_extension(format!("jsonl.{}.tmp", std::process::id()))
/// }
/// ```
///
/// For input `issues.jsonl`, `Path::with_extension` strips `.jsonl` and appends
/// the new extension, producing `issues.jsonl.<pid>.tmp`. The PID is decimal
/// digits; there is NO random suffix. We deliberately match strictly so we
/// never touch SQLite sidecars (`-wal`, `-shm`) or the live `issues.jsonl`.
fn is_jsonl_temp_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("issues.jsonl.") else {
        return false;
    };
    let Some(pid_str) = rest.strip_suffix(".tmp") else {
        return false;
    };
    !pid_str.is_empty() && pid_str.bytes().all(|b| b.is_ascii_digit())
}

/// Sweep stale jsonl temp files older than `min_age`. Returns count removed.
/// Caller MUST hold .write.lock when calling this; we do not acquire it here.
pub(crate) fn sweep_stale_jsonl_temps(
    beads_dir: &Path,
    min_age: Duration,
) -> std::io::Result<u64> {
    let now = SystemTime::now();
    let mut removed = 0;
    let entries = match std::fs::read_dir(beads_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !is_jsonl_temp_file(name_str) { continue; }
        // TOCTOU: the file may have been renamed/unlinked between read_dir and
        // metadata (e.g., a concurrent atomic write completing). Skip the entry
        // instead of failing the whole sweep.
        let Ok(meta) = entry.metadata() else { continue; };
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = now.duration_since(modified) {
                if age >= min_age {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ignores_wal_and_shm_sidecars_and_live_jsonl() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("issues.jsonl"), b"x").unwrap();
        std::fs::write(dir.path().join("issues.jsonl-wal"), b"x").unwrap();
        std::fs::write(dir.path().join("issues.jsonl-shm"), b"x").unwrap();
        std::fs::write(dir.path().join("beads.db-wal"), b"x").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 0);
        assert!(dir.path().join("issues.jsonl").exists());
        assert!(dir.path().join("issues.jsonl-wal").exists());
        assert!(dir.path().join("beads.db-wal").exists());
    }

    #[test]
    fn removes_old_jsonl_tmp_files() {
        let dir = TempDir::new().unwrap();
        // beads_rust temp scheme: `issues.jsonl.<pid>.tmp` (decimal digits only)
        let p = dir.path().join("issues.jsonl.12345.tmp");
        std::fs::write(&p, b"orphan").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 1);
        assert!(!p.exists());
    }

    #[test]
    fn keeps_recent_tmp_files() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("issues.jsonl.99999.tmp");
        std::fs::write(&p, b"in-flight").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::from_secs(3600)).unwrap();
        assert_eq!(removed, 0);
        assert!(p.exists());
    }

    #[test]
    fn ignores_non_pid_lookalikes() {
        let dir = TempDir::new().unwrap();
        // Right shape but non-digit middle — must not be matched.
        let p1 = dir.path().join("issues.jsonl.abc.tmp");
        // Right prefix/suffix but empty middle.
        let p2 = dir.path().join("issues.jsonl..tmp");
        std::fs::write(&p1, b"x").unwrap();
        std::fs::write(&p2, b"x").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 0);
        assert!(p1.exists());
        assert!(p2.exists());
    }

    #[test]
    fn tolerates_missing_dir() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("does-not-exist");
        assert_eq!(sweep_stale_jsonl_temps(&nonexistent, Duration::ZERO).unwrap(), 0);
    }
}
```

- [ ] **Step 2: Run tests + commit**

Run: `cargo test -p spur-pm beads_crate::init --lib`
Expected: 6 pass (FS detection + 5 sweep tests including the lookalike guard).

```bash
git add crates/spur-pm/src/beads_crate/init.rs
git commit -m "spur-pm: add sweep_stale_jsonl_temps init guard"
```

---

### Task 8: Migration-under-flock helper

> **Plan revision (2026-05-05) - bd-5z7w:** The lock-contract split
> between `open_writer_under_migration_lock` and
> `detect_and_force_flush_stale_jsonl` was real and was deferred per
> bd-5z7w until adapter wiring made the call graph concrete. The fix is
> design option B: `init::init_writer_with_flush`, a combined operation
> that holds `.write.lock` across SQLite open, stale JSONL temp sweep, and
> `sync::auto_flush`. The misleading
> `force_flush_on_fresh_db_is_no_op` test was replaced by
> `init_writer_with_flush_runs_clean_on_fresh_dir`. Adapter-level lock
> timeout coverage lives in
> `crates/spur-pm/tests/init_writer_with_flush_lock.rs`; no `#[ignore]`
> deferral was needed because upstream `beads_rust::sync` same-process
> tests show the held `std::fs::File` lock blocks a second acquisition.

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/init.rs`

> **API note (verified 2026-05-05 against beads_rust 0.2.1):**
> - `SqliteStorage::open_with_timeout(path: &Path, lock_timeout_ms: Option<u64>) -> Result<Self>` — `path` is the **`.db` file path**, not the `.beads` directory. Pass `&beads_dir.join("beads.db")`.
> - `sync::blocking_write_lock_with_timeout(beads_dir: &Path, lock_timeout_ms: Option<u64>) -> Result<File>` — takes the **`.beads` directory** and joins `.write.lock` internally. Returns the lock `File` which MUST be kept in scope for the lifetime of the lock; dropping it releases the flock silently.

- [ ] **Step 1: Add the migration helper**

Append:

```rust
use beads_rust::storage::sqlite::SqliteStorage;
use beads_rust::sync;

/// Open the writer connection with cross-process serialization. Holds
/// `.beads/.write.lock` for the duration of schema/migration work.
///
/// Returns the opened `SqliteStorage` ready for writes. The flock is released
/// once the storage is fully initialized (the caller's later writes will
/// re-acquire it per-write).
#[allow(dead_code)] // wired into Section C/D adapter open path
pub(crate) fn open_writer_under_migration_lock(
    beads_dir: &Path,
    lock_timeout_ms: u64,
) -> anyhow::Result<SqliteStorage> {
    // Acquire .write.lock — guards schema migration against other instances
    // racing the same first-open. `_guard: File` must stay in scope until we
    // return; dropping it releases the flock.
    let _guard = sync::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms))?;
    // Opening the storage runs schema init / migrations as needed.
    let db_path = beads_dir.join("beads.db");
    let storage = SqliteStorage::open_with_timeout(&db_path, Some(lock_timeout_ms))?;
    // _guard drops here, releasing flock.
    Ok(storage)
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn opens_writer_in_fresh_dir() {
        let dir = TempDir::new().unwrap();
        let _writer = open_writer_under_migration_lock(dir.path(), 5_000)
            .expect("first open should succeed");
        // Subsequent open in the same process should also succeed
        let _writer2 = open_writer_under_migration_lock(dir.path(), 5_000)
            .expect("second open should succeed");
    }
}
```

- [ ] **Step 2: Run tests + commit**

Run: `cargo test -p spur-pm beads_crate::init --lib`
Expected: 7 pass (FS + 5 sweep + 1 migration).

```bash
git add crates/spur-pm/src/beads_crate/init.rs
git commit -m "spur-pm: add open_writer_under_migration_lock"
```

---

### Task 9: Stale-JSONL boot detection

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/init.rs`

> **API correction (verified 2026-05-05 against beads_rust 0.2.1):**
> The actual `sync::auto_flush` signature is:
>
> ```rust
> pub fn auto_flush(
>     storage: &mut SqliteStorage,
>     beads_dir: &Path,
>     jsonl_path: &Path,
>     allow_external_jsonl: bool,
> ) -> Result<AutoFlushResult>
> ```
>
> There is **no `ExportConfig` / `AutoFlushConfig`** parameter. Pass `false` for `allow_external_jsonl` (matches our single-writer assumption — we do not allow other tools to mutate the JSONL). The internal `ExportConfig` is constructed by `auto_flush` itself.

- [ ] **Step 1: Add stale-JSONL detection**

Append:

```rust
/// Detect whether the SQLite db has writes that haven't been flushed to JSONL,
/// and force a flush if so. Returns the `AutoFlushResult` for inspection.
///
/// # Lock contract
///
/// Caller MUST hold `.write.lock` for the duration of the call. `auto_flush`
/// reads dirty rows from SQLite and rewrites the JSONL file; without a lock,
/// a concurrent writer could interleave and corrupt the JSONL atomic-write.
///
/// Note that `open_writer_under_migration_lock` releases its flock on return.
/// The intended Section C/D wiring is:
///   1. Acquire `sync::blocking_write_lock_with_timeout(beads_dir, …)`.
///   2. Call `detect_and_force_flush_stale_jsonl` while holding it.
///   3. Drop the lock.
///
/// Tracked in bd-5z7w (T8/T9 lock-contract design follow-up).
#[allow(dead_code)] // wired into Section C/D adapter open path
pub(crate) fn detect_and_force_flush_stale_jsonl(
    storage: &mut SqliteStorage,
    beads_dir: &Path,
    jsonl_path: &Path,
) -> anyhow::Result<beads_rust::sync::AutoFlushResult> {
    // beads_rust auto_flush is idempotent: it computes whether the JSONL is
    // out of sync with the SQLite db, and is a no-op if not. We call it
    // unconditionally at boot. `allow_external_jsonl: false` because spur-pm
    // is the single writer to the JSONL file in our model.
    let result = sync::auto_flush(storage, beads_dir, jsonl_path, false)?;
    Ok(result)
}
```

- [ ] **Step 2: Add minimal test**

In the existing `mod migration_tests`, append:

```rust
#[test]
fn force_flush_on_fresh_db_is_no_op() {
    let dir = TempDir::new().unwrap();
    let mut storage = open_writer_under_migration_lock(dir.path(), 5_000).unwrap();
    let jsonl = dir.path().join("issues.jsonl");
    let result =
        detect_and_force_flush_stale_jsonl(&mut storage, dir.path(), &jsonl).unwrap();
    // Fresh db has no dirty rows; auto_flush must report no work done.
    assert!(!result.flushed, "fresh db should not trigger a flush");
    assert_eq!(result.exported_count, 0, "no rows to export on fresh db");
}
```

- [ ] **Step 3: Run tests + commit**

Run: `cargo test -p spur-pm beads_crate::init --lib`
Expected: 8 pass (FS + 5 sweep + 2 migration).

```bash
git add crates/spur-pm/src/beads_crate/init.rs
git commit -m "spur-pm: add detect_and_force_flush_stale_jsonl init guard"
```

---

## Section C — Adapter shape (Tasks 10-14)

### Task 10: Adapter struct + open()

> **Plan revision (2026-05-05) — major:** beads_rust 0.2.1's
> `SqliteStorage` is `!Send` (fsqlite uses `Rc<RefCell<…>>`). The
> original plan's `Arc<Mutex<SqliteStorage>>` writer + persistent
> `Arc<ReaderPool>` shape cannot be moved into `tokio::task::spawn_blocking`
> from a multi-threaded runtime. Beads_rust's own MCP module solves this
> by NOT holding storage open: `BeadsState` stores only paths, and each
> handler call opens a fresh `SqliteStorage` (see beads_rust 0.2.1
> `src/mcp/mod.rs:85-107`). We adopt the same pattern. The
> `ReaderPool` from Task 4 is left in place as a future optimization
> hook (single-thread / `LocalSet` callers can still use it directly)
> but is NOT held by the adapter. `BeadsCrateAdapter` now stores
> `beads_dir`, `jsonl_path`, `config`, and `metrics` only — all
> `Send + Sync`. bd-5z7w later supersedes the split flush helper with
> `init::init_writer_with_flush`; see the following callout.

> **Plan revision (2026-05-05) - bd-5z7w:** The Section C `open()` wiring
> must not compose separate helpers that release `.write.lock` before
> `auto_flush`. The adapter now calls `init::init_writer_with_flush(...)`
> once inside the existing `spawn_blocking` init guard, after
> `detect_local_fs` when non-local filesystems are disallowed. The helper
> preserves best-effort stale temp sweeping but propagates `auto_flush`
> errors so JSONL corruption is surfaced. The old
> `force_flush_on_fresh_db_is_no_op` test was removed in favor of the
> combined-helper test, and
> `crates/spur-pm/tests/init_writer_with_flush_lock.rs` covers lock
> timeout behavior without an ignored cross-process deferral.

**Files:**
- Create: `crates/spur-pm/src/beads_crate/adapter.rs`
- Modify: `crates/spur-pm/src/beads_crate/mod.rs`

- [ ] **Step 1: Write skeleton**

Create `crates/spur-pm/src/beads_crate/adapter.rs`:

```rust
//! `BeadsCrateAdapter` — direct-linkage adapter to `beads_rust` 0.2.1.
//!
//! Open-fresh-per-call shape: `SqliteStorage` is `!Send` (fsqlite uses
//! `Rc<RefCell<…>>`), so we mirror beads_rust's own MCP module —
//! store only paths and config in the adapter, open a new `SqliteStorage`
//! inside each `spawn_blocking` invocation, and drop it when the closure
//! returns. The closure result must be `Send`, but the storage itself
//! never crosses thread boundaries.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::beads_crate::backoff::BackoffPolicy;
use crate::beads_crate::init;
use crate::beads_crate::metrics::ContentionMetrics;

#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub lock_timeout_ms: u64,
    pub stale_tmp_min_age: Duration,
    pub allow_non_local_fs: bool,
    pub backoff: BackoffPolicy,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            lock_timeout_ms: 5_000,
            stale_tmp_min_age: Duration::from_secs(3600),
            allow_non_local_fs: false,
            backoff: BackoffPolicy::default(),
        }
    }
}

pub struct BeadsCrateAdapter {
    pub(crate) beads_dir: PathBuf,
    pub(crate) jsonl_path: PathBuf,
    pub(crate) config: AdapterConfig,
    pub(crate) metrics: Arc<ContentionMetrics>,
}

impl BeadsCrateAdapter {
    pub async fn open(beads_dir: &Path, config: AdapterConfig) -> anyhow::Result<Self> {
        let beads_dir = beads_dir.to_path_buf();
        let jsonl_path = beads_dir.join("issues.jsonl");

        let dir_for_init = beads_dir.clone();
        let jsonl_for_init = jsonl_path.clone();
        let cfg_for_init = config.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if !cfg_for_init.allow_non_local_fs {
                init::detect_local_fs(&dir_for_init)?;
            }
            // open_writer_under_migration_lock returns a SqliteStorage that we
            // use ONLY for boot-time work, then drop on this thread. It never
            // crosses thread boundaries.
            let mut writer = init::open_writer_under_migration_lock(
                &dir_for_init,
                cfg_for_init.lock_timeout_ms,
            )?;
            let _ = init::sweep_stale_jsonl_temps(&dir_for_init, cfg_for_init.stale_tmp_min_age);
            let _ = init::detect_and_force_flush_stale_jsonl(
                &mut writer,
                &dir_for_init,
                &jsonl_for_init,
            );
            Ok(())
        })
        .await??;

        let metrics = Arc::new(ContentionMetrics::default());

        Ok(Self {
            beads_dir,
            jsonl_path,
            config,
            metrics,
        })
    }

    pub fn metrics(&self) -> &ContentionMetrics {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test(flavor = "multi_thread")]
    async fn open_succeeds_in_fresh_dir() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .expect("adapter opens");
        assert!(adapter.beads_dir.exists());
    }
}
```

- [ ] **Step 2: Wire module**

In `crates/spur-pm/src/beads_crate/mod.rs`, add:

```rust
pub mod adapter;
pub use adapter::{AdapterConfig, BeadsCrateAdapter};
```

- [ ] **Step 3: Run tests + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib`
Expected: 1 pass.

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: BeadsCrateAdapter::open with init guards wired"
```

---

### Task 11: `read` primitive

> **Plan revision (2026-05-05):** Two changes from original plan:
> (1) `SqliteStorage::list_filters_count` doesn't exist — use
> `count_issues()`. (2) The reader-pool checkout is replaced with
> open-fresh-per-call (T10 architectural revision): each `read` opens
> a new `SqliteStorage` inside `spawn_blocking`, runs the closure, and
> drops the storage on the same blocking thread. The closure result
> must be `Send`, but the storage never crosses thread boundaries.

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add the primitive**

In `impl BeadsCrateAdapter`, add:

```rust
/// Lock-free snapshot read. Opens a fresh `SqliteStorage` connection
/// for the duration of `f` and drops it on return. WAL mode gives
/// snapshot isolation across concurrent readers and writers.
pub async fn read<T, F>(&self, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let metrics = Arc::clone(&self.metrics);
    let db_path = self.beads_dir.join("beads.db");
    tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
        metrics.incr_read();
        let storage = beads_rust::storage::sqlite::SqliteStorage::open(&db_path)?;
        f(&storage)
    })
    .await?
}
```

- [ ] **Step 2: Add a test**

In the existing `mod tests`, append:

```rust
#[tokio::test]
async fn read_returns_storage_value() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let count = adapter.read(|s| Ok(s.count_issues()?)).await.unwrap();
    assert_eq!(count, 0);
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib`
Expected: 2 pass.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: BeadsCrateAdapter::read primitive"
```

---

### Task 12: `write` primitive (with backoff)

> **Plan revision (2026-05-05):** Three changes:
> (1) Open-fresh-per-call (consistent with T10): the writer is opened
> inside `spawn_blocking` AFTER `.write.lock` is acquired, used for the
> closure, and dropped before the lock is released. No persistent
> `Arc<Mutex<SqliteStorage>>`.
> (2) `Issue` has no `new` constructor and doesn't derive `Default`; the
> smoke-test uses an `_s` closure that returns a constant. Real Issue
> construction lives in Section D (IssueTracker).
> (3) `ContentionMetrics::write_total` is `AtomicU64`, not `AtomicUsize`.

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add helper for flock-with-backoff**

In `crates/spur-pm/src/beads_crate/adapter.rs`, above the impl, add:

```rust
use std::time::Instant;
use beads_rust::sync;

fn acquire_write_lock_with_backoff(
    beads_dir: &Path,
    backoff: &BackoffPolicy,
    lock_timeout_ms: u64,
    metrics: &ContentionMetrics,
) -> anyhow::Result<std::fs::File> {
    let start = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        let attempt_start = Instant::now();
        match sync::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms)) {
            Ok(file) => {
                metrics.record_lock_wait(attempt_start.elapsed());
                return Ok(file);
            }
            Err(_) => {
                metrics.incr_busy();
                let elapsed = start.elapsed();
                let rand = fastrand_unit();
                let Some(delay) = backoff.step(attempt, elapsed, rand) else {
                    metrics.incr_ceiling();
                    anyhow::bail!("write lock acquisition exceeded ceiling after {:?}", elapsed);
                };
                std::thread::sleep(delay);
                attempt += 1;
            }
        }
    }
}

fn fastrand_unit() -> f64 {
    // simple LCG-based unit-interval RNG; avoids adding a rand dependency
    use std::cell::Cell;
    thread_local! { static SEED: Cell<u64> = Cell::new(0x9E3779B97F4A7C15); }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        ((x as u32) as f64) / (u32::MAX as f64)
    })
}
```

- [ ] **Step 2: Add the `write` primitive**

In `impl BeadsCrateAdapter`, add:

```rust
/// Single write under cross-process flock. Opens a fresh
/// `SqliteStorage` AFTER acquiring `.write.lock`, runs the closure,
/// drops both. Each `beads_rust` mutation method (update_issue, etc.)
/// is internally atomic via the crate's `with_write_transaction`.
pub async fn write<T, F>(&self, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&mut beads_rust::storage::sqlite::SqliteStorage) -> anyhow::Result<T>
        + Send
        + 'static,
    T: Send + 'static,
{
    let metrics = Arc::clone(&self.metrics);
    let beads_dir = self.beads_dir.clone();
    let backoff = self.config.backoff.clone();
    let lock_timeout_ms = self.config.lock_timeout_ms;
    tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
        let _flock = acquire_write_lock_with_backoff(
            &beads_dir,
            &backoff,
            lock_timeout_ms,
            &metrics,
        )?;
        let mut storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
            &beads_dir.join("beads.db"),
            Some(lock_timeout_ms),
        )?;
        metrics.incr_write();
        let result = f(&mut storage);
        if result.is_err() {
            metrics.incr_write_error();
        }
        result
    })
    .await?
}
```

- [ ] **Step 3: Add test**

In `mod tests`, append:

```rust
#[tokio::test]
async fn write_runs_closure_under_flock() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let result: i32 = adapter.write(|_s| Ok(42)).await.unwrap();
    assert_eq!(result, 42);
    assert_eq!(
        adapter.metrics().write_total.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib`
Expected: 3 pass.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: BeadsCrateAdapter::write primitive with backoff"
```

---

### Task 13: `batch` primitive

> **Plan revision (2026-05-05):** Same `Issue` constructor drift as T12.
> The smoke-test runs an N-step closure that just counts iterations and
> verifies a single `write_total` increment for the whole batch.

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add primitive**

In `impl BeadsCrateAdapter`, add:

```rust
/// Multiple mutations under one flock acquisition. NOT a single DB
/// transaction — each individual call inside `f` is atomic, but the batch
/// as a whole is not. See spec "Multi-statement atomicity" non-goal.
pub async fn batch<T, F>(&self, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&mut SqliteStorage) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    // Same shape as `write`; the API distinction signals intent to the caller.
    self.write(f).await
}
```

- [ ] **Step 2: Add test**

```rust
#[tokio::test]
async fn batch_runs_multiple_steps_under_one_lock() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let count = adapter.batch(|_s| {
        let mut acc = 0_usize;
        for _ in 0..5 {
            acc += 1;
        }
        Ok(acc)
    }).await.unwrap();
    assert_eq!(count, 5);
    // Single batch = single write op recorded
    assert_eq!(
        adapter.metrics().write_total.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib`
Expected: 4 pass.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: BeadsCrateAdapter::batch primitive"
```

---

### Task 14: Snapshot CAS pair (`read_snapshot` + `validate_and_commit`)

> **Plan revision (2026-05-05):** Three changes:
> (1) beads_rust 0.2.1 has no public `connection_pragma` /
> `data_version` accessor. Use `count_issues()` as a coarse proxy:
> detects net add/delete between snapshot and commit, misses pure
> field updates. Documented in code + follow-up to upstream
> `PRAGMA data_version`.
> (2) Open-fresh-per-call (consistent with T10): `read_snapshot`
> opens a fresh `SqliteStorage` for the read; `validate_and_commit`
> opens a fresh `SqliteStorage` after acquiring `.write.lock`.
> (3) Two tests: a no-conflict happy path AND a conflict-detection
> regression test that simulates a concurrent net-add via direct
> `Issue` struct-literal construction (the CAS rejection path is the
> safety property worth locking).

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add helpers and primitives**

```rust
use crate::beads_crate::snapshot::{Conflict, Snapshot};

/// Coarse data_version proxy. beads_rust 0.2.1 does not expose
/// `PRAGMA data_version`; until it does, we use `count_issues()`. This
/// detects net add/delete between snapshot and commit, which covers the
/// IssueTracker CAS use cases (e.g. "delete iff still present"). It
/// MISSES pure field updates that don't change the row count — callers
/// who need that level of strictness should not rely on this proxy yet.
/// Follow-up: expose PRAGMA data_version upstream or vendor a helper.
fn read_data_version(s: &SqliteStorage) -> anyhow::Result<i64> {
    let count = s.count_issues()?;
    Ok(count as i64)
}

impl BeadsCrateAdapter {
    /// Read a snapshot value + record the data_version so the caller can do
    /// async work and then call `validate_and_commit` later.
    pub async fn read_snapshot<S, F>(&self, f: F) -> anyhow::Result<Snapshot<S>>
    where
        F: FnOnce(&SqliteStorage) -> anyhow::Result<S> + Send + 'static,
        S: Send + 'static,
    {
        let pool = Arc::clone(&self.reader_pool);
        let metrics = Arc::clone(&self.metrics);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Snapshot<S>> {
            metrics.incr_read();
            let guard = pool.checkout()?;
            let storage = guard.storage();
            let value = f(storage)?;
            let data_version = read_data_version(storage)?;
            Ok(Snapshot { value, data_version })
        }).await?
    }

    /// Apply a write conditioned on the snapshot's data_version still matching.
    /// Returns Conflict if state moved between read and validate.
    pub async fn validate_and_commit<S, T, FW>(
        &self,
        snapshot: Snapshot<S>,
        write: FW,
    ) -> anyhow::Result<T>
    where
        FW: FnOnce(&mut SqliteStorage, S) -> anyhow::Result<T> + Send + 'static,
        S: Send + 'static,
        T: Send + 'static,
    {
        let writer = Arc::clone(&self.writer);
        let metrics = Arc::clone(&self.metrics);
        let beads_dir = self.beads_dir.clone();
        let backoff = self.config.backoff.clone();
        let lock_timeout_ms = self.config.lock_timeout_ms;
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            let _flock = acquire_write_lock_with_backoff(&beads_dir, &backoff, lock_timeout_ms, &metrics)?;
            let mut writer_guard = writer.blocking_lock();
            let current = read_data_version(&writer_guard)?;
            if current != snapshot.data_version {
                metrics.incr_conflict();
                return Err(anyhow::anyhow!(Conflict::data_version(snapshot.data_version, current)));
            }
            metrics.incr_write();
            let result = write(&mut writer_guard, snapshot.value);
            if result.is_err() { metrics.incr_write_error(); }
            result
        }).await?
    }
}
```

- [ ] **Step 2: Add test**

```rust
#[tokio::test]
async fn read_snapshot_then_validate_and_commit_no_conflict() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    // Snapshot the empty db (count_issues == 0).
    let snap = adapter.read_snapshot(|_s| Ok(())).await.unwrap();
    // Validate-and-commit without any concurrent writers — should succeed
    // because data_version proxy hasn't moved.
    let result: i32 = adapter.validate_and_commit(snap, |_s, _| Ok(7)).await.unwrap();
    assert_eq!(result, 7);
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib`
Expected: 5 pass.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: snapshot CAS pair (read_snapshot + validate_and_commit)"
```

---

## Section D — IssueTracker impl (Tasks 15-20)

### Task 15: Implement `IssueTracker::get_issue`

**Files:**
- Create: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

> **Plan revision (2026-05-05):** T15 lands an inherent
> `BeadsCrateAdapter::get_issue` plus `br_to_pm_issue`, not a partial
> `IssueTracker` impl. Apply the audited drift fixes: use `Status` and
> `IssueType` Display via `.to_string()`, read `Priority` through `.0`,
> handle `SqliteStorage::get_issue` returning `Ok(None)` as not found, and
> construct test `beads_rust::model::Issue` values with a struct literal because
> `BrIssue::new` does not exist.

- [ ] **Step 1: Type-conversion helpers**

Create `crates/spur-pm/src/beads_crate/issue_tracker.rs`:

```rust
//! `IssueTracker` trait impl for `BeadsCrateAdapter`. Maps between the
//! generic `spur_pm::types` and the `beads_rust::model` types.

use async_trait::async_trait;
use chrono::Utc;

use crate::adapter::IssueTracker;
use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::types::{Issue, IssueCreate, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource};

fn br_to_pm_issue(br: beads_rust::model::Issue) -> Issue {
    Issue {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        body: br.description.unwrap_or_default(),
        status: format!("{:?}", br.status).to_lowercase(), // adjust mapping if enum repr differs
        labels: br.labels,
        assignee: br.assignee,
        url: format!("beads://{}", "ID_PLACEHOLDER"), // fix to use actual id; see step 2
        priority: Some(br.priority as i32),
        issue_type: Some(format!("{:?}", br.issue_type).to_lowercase()),
        blocked_by: vec![], // populate from deps if needed
        due_at: br.due_at,
        created_at: br.created_at,
        updated_at: br.updated_at,
    }
}
```

(The exact field names need to match `beads_rust::model::Issue`; adjust the `br.status`/`br.issue_type` mapping based on how the crate represents enums.)

- [ ] **Step 2: Fix the `url` to use the right id**

Replace the `url:` line:

```rust
url: format!("beads://{}", br_id_for_url),  // where br_id_for_url = clone of br.id before move
```

Restructure to clone the id before the struct move; or clone in the helper:

```rust
fn br_to_pm_issue(br: beads_rust::model::Issue) -> Issue {
    let url = format!("beads://{}", br.id);
    Issue {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        body: br.description.unwrap_or_default(),
        status: format!("{:?}", br.status).to_lowercase(),
        labels: br.labels,
        assignee: br.assignee,
        url,
        priority: Some(br.priority as i32),
        issue_type: Some(format!("{:?}", br.issue_type).to_lowercase()),
        blocked_by: vec![],
        due_at: br.due_at,
        created_at: br.created_at,
        updated_at: br.updated_at,
    }
}
```

- [ ] **Step 3: Implement get_issue**

Append:

```rust
#[async_trait]
impl IssueTracker for BeadsCrateAdapter {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        let id = id.to_string();
        self.read(move |s| {
            let br = s.get_issue(&id).map_err(anyhow::Error::from)?;
            Ok(br_to_pm_issue(br))
        }).await
    }

    // stubs for the rest — filled in by subsequent tasks
    async fn list_issues(&self, _filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> { unimplemented!() }
    async fn create_issue(&self, _params: IssueCreate) -> anyhow::Result<String> { unimplemented!() }
    async fn update_issue(&self, _id: &str, _update: IssueUpdate) -> anyhow::Result<()> { unimplemented!() }
    async fn add_dependency(&self, _issue_id: &str, _depends_on_id: &str) -> anyhow::Result<()> { unimplemented!() }
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> { unimplemented!() }
}
```

- [ ] **Step 4: Wire module + smoke test**

In `crates/spur-pm/src/beads_crate/mod.rs`:

```rust
pub mod issue_tracker;
```

In `mod tests` of `adapter.rs`, append:

```rust
#[tokio::test]
async fn get_issue_round_trips_via_trait() {
    use crate::adapter::IssueTracker;
    use beads_rust::model::Issue as BrIssue;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let id_returned = adapter.write(|s| {
        let mut issue = BrIssue::new("Hello".to_string());
        let id_before = issue.id.clone(); // adapt if id is assigned by create
        s.create_issue(&issue, "test").map_err(anyhow::Error::from)?;
        Ok(id_before)
    }).await.unwrap();
    let pm_issue = adapter.get_issue(&id_returned).await.unwrap();
    assert_eq!(pm_issue.title, "Hello");
    assert_eq!(pm_issue.source, PmSource::Beads);
}
```

(If `Issue::new` doesn't auto-generate an ID, use whatever the crate provides — many beads APIs return the id from `create_issue` directly; adjust accordingly.)

- [ ] **Step 5: Run + commit**

Run: `cargo test -p spur-pm beads_crate --lib`
Expected: all primitives' tests pass + 1 round-trip test.

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: IssueTracker::get_issue via crate adapter"
```

---

### Task 16: Implement `IssueTracker::list_issues`

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

- [ ] **Step 1: Convert filter and call list**

Replace the `list_issues` stub with:

```rust
async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
    let filter = filter.clone();
    self.read(move |s| {
        // Map IssueFilter → beads_rust::storage::sqlite::ListFilters
        let mut br_filters = beads_rust::storage::sqlite::ListFilters::default();
        br_filters.labels = filter.labels.clone();
        br_filters.status = filter.status.clone();
        br_filters.assignee = filter.assignee.clone();
        br_filters.limit = filter.limit;
        br_filters.offset = filter.offset;
        br_filters.include_closed = filter.include_closed;
        // (Map other fields as the ListFilters API permits.)

        let issues = s.list_issues(&br_filters).map_err(anyhow::Error::from)?;
        let summaries: Vec<IssueSummary> = issues.into_iter().map(|br| {
            IssueSummary {
                id: br.id.clone(),
                source: PmSource::Beads,
                title: br.title,
                status: format!("{:?}", br.status).to_lowercase(),
                labels: br.labels,
                url: format!("beads://{}", br.id),
                priority: Some(br.priority as i32),
                issue_type: Some(format!("{:?}", br.issue_type).to_lowercase()),
                assignee: br.assignee,
            }
        }).collect();
        Ok(summaries)
    }).await
}
```

- [ ] **Step 2: Add test**

In `mod tests` of `adapter.rs`, append:

```rust
#[tokio::test]
async fn list_issues_returns_seeded_data() {
    use crate::adapter::IssueTracker;
    use beads_rust::model::Issue as BrIssue;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    adapter.batch(|s| {
        for i in 0..3 {
            s.create_issue(&BrIssue::new(format!("T{i}")), "test").map_err(anyhow::Error::from)?;
        }
        Ok(())
    }).await.unwrap();
    let summaries = adapter.list_issues(IssueFilter::default()).await.unwrap();
    assert_eq!(summaries.len(), 3);
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate --lib`
Expected: list test passes.

```bash
git add crates/spur-pm/src/beads_crate/issue_tracker.rs crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: IssueTracker::list_issues via crate adapter"
```

> **Plan revision (2026-05-05):** Section D drift fixes applied to T16:
> - `ListFilters` field is `statuses: Option<Vec<Status>>` (not `status: Option<String>`); parse via `Status::from_str` and wrap in single-element `Vec`.
> - `ListFilters.labels` is `Option<Vec<String>>` (not `Vec<String>`); set to `Some(filter.labels.clone())` only when non-empty.
> - `ListFilters` has no `priority_min`/`priority_max`; expand to `priorities: Option<Vec<Priority>>` over the inclusive range.
> - `IssueFilter.since` maps to `ListFilters.updated_after`.
> - Map `IssueFilter.issue_type` (string) → `ListFilters.types: Option<Vec<IssueType>>` via `IssueType::from_str`.
> - Map `IssueFilter.text_search` → `ListFilters.title_contains`.
> - Implementation lives as inherent method on `BeadsCrateAdapter` (matching T15 shape); trait impl block materializes in T20.
> - Test uses `minimal_issue` struct-literal helper from T15 (no `BrIssue::new` constructor exists in beads_rust 0.2.1).

---

### Task 17: Implement `IssueTracker::create_issue`

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

- [ ] **Step 1: Replace stub**

```rust
async fn create_issue(&self, params: IssueCreate) -> anyhow::Result<String> {
    self.write(move |s| {
        let mut issue = beads_rust::model::Issue::new(params.title);
        if let Some(desc) = params.description { issue.description = Some(desc); }
        if let Some(p) = params.priority { issue.priority = p as u8; }
        for l in params.labels { issue.labels.push(l); }
        if let Some(a) = params.assignee { issue.assignee = Some(a); }
        // issue_type: map string → enum. If Issue::issue_type is an enum:
        if let Some(t) = params.issue_type {
            issue.issue_type = match t.to_lowercase().as_str() {
                "epic" => beads_rust::model::IssueType::Epic,
                "bug" => beads_rust::model::IssueType::Bug,
                "feature" => beads_rust::model::IssueType::Feature,
                _ => beads_rust::model::IssueType::Task,
            };
        }
        let new_id = issue.id.clone();
        s.create_issue(&issue, "spur").map_err(anyhow::Error::from)?;
        // Wire parent / depends_on as dependency edges
        if let Some(parent) = params.parent {
            s.add_dependency(&new_id, &parent, "spur").map_err(anyhow::Error::from)?;
        }
        for dep in params.depends_on {
            s.add_dependency(&new_id, &dep, "spur").map_err(anyhow::Error::from)?;
        }
        Ok(new_id)
    }).await
}
```

(Adjust enum names + method signatures to match crate.)

- [ ] **Step 2: Add test + commit**

```rust
#[tokio::test]
async fn create_issue_returns_id() {
    use crate::adapter::IssueTracker;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let id = adapter.create_issue(IssueCreate {
        title: "Hello".into(),
        description: Some("Body".into()),
        priority: Some(1),
        labels: vec!["test".into()],
        ..Default::default()
    }).await.unwrap();
    assert!(!id.is_empty());
    let fetched = adapter.get_issue(&id).await.unwrap();
    assert_eq!(fetched.title, "Hello");
}
```

Run: `cargo test -p spur-pm beads_crate --lib`

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: IssueTracker::create_issue via crate adapter"
```

> **Plan revision (2026-05-05):** Section D drift fixes applied to T17:
> - `Issue::new(title)` does not exist in beads_rust 0.2.1; build via struct literal (mirror `minimal_issue` test helper from T15).
> - `Issue::id` must be set explicitly — there is no auto-generation in `s.create_issue()`. Use `beads_rust::util::generate_id(title, description, creator, created_at)`.
> - `priority` is `Priority(pub i32)` newtype — wrap with `Priority(p)`, not `p as u8`.
> - `issue_type` is the `IssueType` enum — parse via `IssueType::from_str` (Custom variant for unknown).
> - Labels are stored out-of-line in beads_rust; the `Issue.labels` field on the struct passed to `s.create_issue()` is **ignored**. Call `s.set_labels(&id, &labels, "spur")` separately after `create_issue` to persist them.
> - `s.add_dependency(issue_id, depends_on_id, dep_type, actor)` — 4 args (not 3). Use `"parent-child"` for parent links and `"blocks"` for `depends_on`.
> - `IssueCreate.estimate_minutes: Option<u32>` → `i32`: use `i32::try_from(m).ok()` to avoid lossy `as` cast.
> - `get_issue` was extended in this commit to fetch labels via `s.get_labels_for_issues(...)` and merge them into the returned Issue (otherwise downstream callers see empty labels).

---

### Task 18: Implement `IssueTracker::update_issue`

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

- [ ] **Step 1: Replace stub**

```rust
async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
    let id = id.to_string();
    self.write(move |s| {
        let mut br_update = beads_rust::storage::sqlite::IssueUpdate::default();
        if let Some(status) = update.status {
            br_update.status = Some(parse_status(&status));  // map string → crate's status enum
        }
        if let Some(p) = update.priority { br_update.priority = Some(p as u8); }
        if let Some(a) = update.assignee {
            br_update.assignee = if a.is_empty() { Some(None) } else { Some(Some(a)) };
        }
        if !update.add_labels.is_empty() { br_update.add_labels = update.add_labels; }
        if !update.remove_labels.is_empty() { br_update.remove_labels = update.remove_labels; }
        s.update_issue(&id, &br_update, "spur").map_err(anyhow::Error::from)?;
        if let Some(comment) = update.comment {
            s.add_comment(&id, &comment, "spur").map_err(anyhow::Error::from)?;
        }
        Ok(())
    }).await
}

fn parse_status(s: &str) -> beads_rust::model::Status {
    match s.to_lowercase().as_str() {
        "open" => beads_rust::model::Status::Open,
        "in_progress" | "inprogress" => beads_rust::model::Status::InProgress,
        "closed" | "done" => beads_rust::model::Status::Closed,
        "blocked" => beads_rust::model::Status::Blocked,
        _ => beads_rust::model::Status::Open,
    }
}
```

(Verify the actual `beads_rust::model::Status` variants and `IssueUpdate` field names; adjust.)

- [ ] **Step 2: Test + commit**

```rust
#[tokio::test]
async fn update_issue_changes_status() {
    use crate::adapter::IssueTracker;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let id = adapter.create_issue(IssueCreate { title: "X".into(), ..Default::default() }).await.unwrap();
    adapter.update_issue(&id, IssueUpdate { status: Some("closed".into()), ..Default::default() }).await.unwrap();
    let after = adapter.get_issue(&id).await.unwrap();
    assert_eq!(after.status, "closed");
}
```

Run + commit:

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: IssueTracker::update_issue via crate adapter"
```

> **Plan revision (2026-05-05):** Section D drift fixes applied to T18:
> - `beads_rust::storage::sqlite::IssueUpdate` has **no** `add_labels`/`remove_labels` fields. Loop over the local `IssueUpdate.add_labels` and call `s.add_label(&id, label, "spur")`; mirror with `s.remove_label(...)`.
> - `IssueUpdate.priority: Option<Priority>` (newtype) — wrap as `Some(Priority(p))`.
> - `IssueUpdate.status: Option<Status>` (enum) — parse via `Status::from_str`.
> - `IssueUpdate.assignee: Option<Option<String>>` — empty-string sentinel maps to `Some(None)` (unassign), non-empty maps to `Some(Some(value))`.
> - `s.add_comment(issue_id, author, text)` — argument order is (id, author/actor, body). The plan snippet had `(id, body, "spur")`.

---

### Task 19: Implement `IssueTracker::add_dependency`

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

- [ ] **Step 1: Replace stub + test + commit**

```rust
async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
    let issue_id = issue_id.to_string();
    let depends_on_id = depends_on_id.to_string();
    self.write(move |s| {
        s.add_dependency(&issue_id, &depends_on_id, "spur").map_err(anyhow::Error::from)
    }).await
}
```

Test in `mod tests`:

```rust
#[tokio::test]
async fn add_dependency_links_two_issues() {
    use crate::adapter::IssueTracker;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let parent = adapter.create_issue(IssueCreate { title: "P".into(), ..Default::default() }).await.unwrap();
    let child = adapter.create_issue(IssueCreate { title: "C".into(), ..Default::default() }).await.unwrap();
    adapter.add_dependency(&child, &parent).await.unwrap();
    // Verify via get_issue.blocked_by once br_to_pm_issue is wired to populate it,
    // OR via a direct list_dependencies query if the crate exposes it.
}
```

Run + commit:

```bash
git add crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: IssueTracker::add_dependency via crate adapter"
```

> **Plan revision (2026-05-05):** Section D drift fixes applied to T19:
> - `s.add_dependency(issue_id, depends_on_id, dep_type, actor)` takes 4 args (not 3) and returns `Result<bool>`. Default `dep_type` is `"blocks"` for the trait's plain `add_dependency` method.
> - The plan's verification idea ("via get_issue.blocked_by") doesn't fire today because `br_to_pm_issue` returns `blocked_by: vec![]` — populating it requires calling `s.get_dependencies_full(&id)` in the read closure. The T19 test instead verifies via `s.get_dependencies(&child)?` directly inside an `adapter.read(...)` call.
> - Future work: extend `br_to_pm_issue` to load deps via `get_dependencies_full` and filter by blocking dependency types (matches `BLOCKING_TYPES` in `crates/spur-pm/src/beads.rs:166`).

---

### Task 20: Implement `IssueTracker::poll`

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

- [ ] **Step 1: Port the existing poll logic**

Read `crates/spur-pm/src/beads.rs` for the existing `poll` implementation (uses `PollCursor` from `beads.rs` line 26). Reuse the `PollCursor` type — move it to a shared location:

- Move the `PollCursor` struct (lines 25-42 in current beads.rs) to `crates/spur-pm/src/poll_cursor.rs` (new file). Both adapters use it.
- In `crates/spur-pm/src/lib.rs`, add `pub mod poll_cursor;` and `pub use poll_cursor::PollCursor;`.
- In `beads_crate/issue_tracker.rs`, port the poll logic to use `BeadsCrateAdapter::list_issues` + `PollCursor::allows`.

```rust
async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
    // Mirror beads.rs poll logic:
    //   1. Read current cursor (kept in inner state)
    //   2. list_issues(since=cursor.ts, include_closed=true, limit=POLL_FETCH_LIMIT)
    //   3. Filter via cursor.allows(id, updated_at)
    //   4. Translate Issue → PmEvent::IssueUpdated
    //   5. Advance cursor to max(updated_at)
    //
    // For now, return empty until cursor state is wired:
    Ok(vec![])
}
```

- [ ] **Step 2: Add cursor state to BeadsCrateAdapter**

In `crates/spur-pm/src/beads_crate/adapter.rs`, add `cursor: tokio::sync::Mutex<PollCursor>` field and initialize it. Then implement the actual poll logic:

```rust
async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
    let mut cursor = self.cursor.lock().await;
    let summaries = self.list_issues(IssueFilter {
        since: Some(cursor.ts),
        include_closed: true,
        limit: Some(POLL_FETCH_LIMIT),
        ..Default::default()
    }).await?;
    // (Convert and update cursor as in beads.rs)
    let mut events = Vec::new();
    let mut new_max = cursor.ts;
    let mut new_ids = std::collections::HashSet::new();
    for sum in summaries {
        let issue = self.get_issue(&sum.id).await?;
        if cursor.allows(&issue.id, issue.updated_at) {
            events.push(PmEvent::IssueUpdated { issue: issue.clone() });
            if issue.updated_at > new_max {
                new_max = issue.updated_at;
                new_ids.clear();
                new_ids.insert(issue.id.clone());
            } else if issue.updated_at == new_max {
                new_ids.insert(issue.id.clone());
            }
        }
    }
    if new_max > cursor.ts || !new_ids.is_empty() {
        cursor.ts = new_max;
        cursor.ids_at_boundary = new_ids;
    }
    Ok(events)
}
```

- [ ] **Step 3: Test (port from existing beads.rs poll tests)**

Add at minimum:

```rust
#[tokio::test]
async fn poll_returns_new_issue_then_empty_on_repoll() {
    use crate::adapter::IssueTracker;
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    adapter.create_issue(IssueCreate { title: "X".into(), ..Default::default() }).await.unwrap();
    let first = adapter.poll().await.unwrap();
    assert_eq!(first.len(), 1);
    let second = adapter.poll().await.unwrap();
    assert!(second.is_empty());
}
```

- [ ] **Step 4: Run + commit**

```bash
git add crates/spur-pm/src/poll_cursor.rs crates/spur-pm/src/lib.rs crates/spur-pm/src/beads_crate/
git commit -m "spur-pm: IssueTracker::poll via crate adapter (shared PollCursor)"
```

> **Plan revision (2026-05-05):** Section D drift fixes applied to T20:
> - `PollCursor` extracted from `crates/spur-pm/src/beads.rs:25-42` to a new `crates/spur-pm/src/poll_cursor.rs`. Both `BeadsAdapter` and `BeadsCrateAdapter` now share it. `lib.rs` re-exports `PollCursor` from the new module (preserves the existing `spur_pm::PollCursor` public path for downstream tests).
> - `BeadsCrateAdapter` gains `cursor: tokio::sync::Mutex<Option<PollCursor>>` (initialized to `None` in `open`).
> - Poll mirrors `BeadsAdapter::poll_with_limit` shape: pull bounded open set via `list_issues(status="open", limit=POLL_FETCH_LIMIT)`, hydrate each summary via `get_issue` to obtain `updated_at`, apply the boundary-safe predicate client-side, and advance the cursor (with the saturation guard preserving the prior cursor on a fully-saturated batch).
> - `POLL_FETCH_LIMIT` continues to live in `beads.rs` (re-used here via `crate::beads::POLL_FETCH_LIMIT`); both adapters share the same constant.
> - The inherent `impl BeadsCrateAdapter { ... }` block in `issue_tracker.rs` was refactored into `#[async_trait] impl IssueTracker for BeadsCrateAdapter`. T15-T19 had each landed methods as inherents on a stub `impl BeadsCrateAdapter`; T20 finalizes the trait impl block and adds the trait `use` to the test module so `adapter.create_issue(...)` etc. resolves.

---

## Section E — Test fixture migration (Tasks 21-25)

### Task 21: Build `TestBeadsWorkspace` helper

**Files:**
- Create: `crates/spur-pm/src/test_workspace.rs`

- [ ] **Step 1: Write the helper**

```rust
//! Test workspace for beads — uses `beads_rust` directly, no `br` CLI.
//! Replaces every `Command::new("br")` test fixture in the codebase.

use std::path::{Path, PathBuf};

use beads_rust::storage::sqlite::SqliteStorage;
use tempfile::TempDir;

pub struct TestBeadsWorkspace {
    _dir: TempDir,
    pub path: PathBuf,
    pub storage: SqliteStorage,
}

impl TestBeadsWorkspace {
    /// Create a fresh, initialized beads workspace in a tempdir.
    pub fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_path_buf();
        let storage = SqliteStorage::open(&path).expect("open beads workspace");
        Self { _dir: dir, path, storage }
    }

    /// Create an issue and return its ID.
    pub fn create_issue(&mut self, title: &str) -> String {
        use beads_rust::model::Issue;
        let issue = Issue::new(title.to_string());
        let id = issue.id.clone();
        self.storage.create_issue(&issue, "test").expect("create");
        id
    }

    pub fn create_epic(&mut self, title: &str) -> String {
        use beads_rust::model::{Issue, IssueType};
        let mut issue = Issue::new(title.to_string());
        issue.issue_type = IssueType::Epic;
        let id = issue.id.clone();
        self.storage.create_issue(&issue, "test").expect("create epic");
        id
    }

    pub fn add_label(&mut self, id: &str, label: &str) {
        let mut update = beads_rust::storage::sqlite::IssueUpdate::default();
        update.add_labels = vec![label.to_string()];
        self.storage.update_issue(id, &update, "test").expect("add label");
    }

    pub fn close_issue(&mut self, id: &str) {
        let mut update = beads_rust::storage::sqlite::IssueUpdate::default();
        update.status = Some(beads_rust::model::Status::Closed);
        self.storage.update_issue(id, &update, "test").expect("close");
    }

    pub fn add_dep(&mut self, child: &str, parent: &str) {
        self.storage.add_dependency(child, parent, "test").expect("dep");
    }

    pub fn path(&self) -> &Path { &self.path }
}
```

(Adjust method names to match crate.)

- [ ] **Step 2: Wire into lib + smoke test**

In `crates/spur-pm/src/lib.rs`:

```rust
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_workspace;
```

(Or just `pub mod test_workspace;` if test files in OTHER crates need to import it; in that case make it always public.)

Add to `crates/spur-pm/Cargo.toml`:

```toml
[features]
test-helpers = []
```

And mark callers' `Cargo.toml` to depend on `spur-pm = { workspace = true, features = ["test-helpers"] }` for tests.

Actually simpler: just make it always-public (no feature gate). Tests in OTHER crates need access; feature-gating across crates is fragile.

So in `crates/spur-pm/src/lib.rs`:

```rust
pub mod test_workspace;
```

Smoke test inline:

```rust
#[cfg(test)]
mod tests {
    use super::test_workspace::TestBeadsWorkspace;
    #[test]
    fn workspace_init_and_create() {
        let mut w = TestBeadsWorkspace::init();
        let id = w.create_issue("Hello");
        assert!(!id.is_empty());
    }
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm test_workspace --lib`
Expected: 1 pass.

```bash
git add crates/spur-pm/src/test_workspace.rs crates/spur-pm/src/lib.rs
git commit -m "spur-pm: add TestBeadsWorkspace helper (no br CLI)"
```

---

### Task 22: Migrate `crates/spur-mcp/src/server.rs` test fixtures

> **Plan revision (2026-05-06):** The original line references drifted:
> 7483/9039/9205/9270 are now 7609/9165/9331/9396 before this task's edit.
> The reusable helper in current `server.rs` is `init_beads_pm`; the migration
> keeps `TestBeadsWorkspace` alive in `PersistedMergeFixture` and uses
> `attach_beads_workspace` to seed the repo `.beads` directory from
> `TestBeadsWorkspace::path()` for the existing PM-service construction path.

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (lines 7483, 9039, 9205, 9270)

- [ ] **Step 1: Identify and rewrite**

For each of the 4 sites, replace the `Command::new("br") ... .args(["init"]) ... .output()` block with `TestBeadsWorkspace::init()`.

Concrete edit for line 7483 area (the `init_beads_pm` helper):

```rust
async fn init_beads_pm(repo: &std::path::Path) -> Arc<spur_pm::PmService> {
    // Initialize beads workspace via direct crate use (no `br` CLI).
    let _storage = beads_rust::storage::sqlite::SqliteStorage::open(repo)
        .expect("init beads workspace");
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}
```

For lines 9039, 9205, 9270, do the analogous replacement (each is a one-shot `br init` in a tempdir).

- [ ] **Step 2: Run server tests**

Run: `cargo test -p spur-mcp --lib server`
Expected: tests still pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "spur-mcp: server.rs test fixtures use beads_rust directly"
```

---

### Task 23: Migrate `crates/spur-mcp/src/plan/reconciler.rs` test fixtures

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs` (lines 2206-2411, 16 sites)

- [ ] **Step 1: Replace each fixture pattern**

Use `TestBeadsWorkspace::init()` for the `br init` calls, and `workspace.create_issue` / `workspace.create_epic` / `workspace.add_label` / `workspace.close_issue` for the `br create` / `br label add` / `br update --status closed` calls.

For example, the block at lines 2222-2286 (Test Epic + Task A/B + label + close):

```rust
let mut w = spur_pm::test_workspace::TestBeadsWorkspace::init();
let epic_id = w.create_epic("Test Epic");
for title in ["Task A", "Task B"] {
    let task_id = w.create_issue(title);
    w.add_label(&task_id, "spur:plan-id:P1");
    w.close_issue(&task_id);
}
w.add_label(&epic_id, "spur:plan-id:P1");
w.add_label(&epic_id, "spur:plan-complete");
w.close_issue(&epic_id);
w.add_label(&epic_id, "spur:integration-pending");
let repo = w.path();
```

Apply the same pattern to all 16 occurrences in this file.

- [ ] **Step 2: Remove the `br_available` helper and the `if !br_available()` skip block**

Since the new path doesn't depend on `br` CLI being installed, drop the early-return guards.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-mcp --lib plan::reconciler`
Expected: tests pass.

```bash
git add crates/spur-mcp/src/plan/reconciler.rs
git commit -m "spur-mcp: reconciler test fixtures use beads_rust directly"
```

---

### Task 24: Migrate test files in `crates/spur-mcp/tests/` and `crates/spur-pm/tests/`

**Files:**
- Modify: ~50 test files (see grep output for `Command::new("br")` matches)

- [ ] **Step 1: Inventory the affected files**

Run: `cargo grep -l 'Command::new("br")' crates/spur-mcp/tests crates/spur-pm/tests`

(Use the standard grep tool if cargo-grep isn't available: `rg -l 'Command::new\("br"\)' crates/spur-mcp/tests/ crates/spur-pm/tests/`.)

Expected list: ~40-50 files.

- [ ] **Step 2: Replace per file**

For each file, the pattern is the same: replace the local `fn run_br(...)` helper and `Command::new("br").args(["init"])` with `spur_pm::test_workspace::TestBeadsWorkspace::init()` and direct method calls.

Common patterns to replace:

| `Command::new("br")` invocation | Replacement |
|---|---|
| `args(["init"])` | `TestBeadsWorkspace::init()` |
| `args(["create", "--type", "task", "--title", t, "--json"])` | `w.create_issue(t)` |
| `args(["create", "--type", "epic", "--title", t, "--json"])` | `w.create_epic(t)` |
| `args(["label", "add", id, label])` | `w.add_label(id, label)` |
| `args(["update", id, "--status", "closed"])` | `w.close_issue(id)` |
| `args(["dep", "add", child, parent])` | `w.add_dep(child, parent)` |

Tip: if a single file has many br calls, do them all in one commit to keep the file in a working state.

- [ ] **Step 3: Run + commit per logical chunk**

Run: `cargo test -p spur-mcp --tests` (and `-p spur-pm --tests`)
Expected: all tests pass.

```bash
git add crates/spur-mcp/tests/ crates/spur-pm/tests/
git commit -m "spur-mcp,spur-pm: migrate test fixtures from br CLI to TestBeadsWorkspace"
```

(If the diff is huge, split per file into multiple commits.)

---

### Task 25: Migrate any other test files using `br`

**Files:**
- Modify: `crates/spur-core/tests/resume_plan_bridge.rs` and any other `br` callers found by grep

- [ ] **Step 1: Final grep**

Run grep tool: `Command::new\("br"\)` across `crates/`.
Expected: only test files; ideally already migrated.

- [ ] **Step 2: Replace + commit**

```bash
git add crates/
git commit -m "tests: complete migration off br CLI in remaining test files"
```

---

## Section F — Wire-in + cleanup (Tasks 26-30)

### Task 26: Replace `BeadsAdapter` in `PmService`

**Files:**
- Modify: `crates/spur-pm/src/service.rs`

- [ ] **Step 1: Read current service.rs to find the construction site**

Identify where `BeadsAdapter::try_new` is called and what config it uses.

- [ ] **Step 2: Swap to `BeadsCrateAdapter::open`**

Replace the `BeadsAdapter::try_new(...)` call with `BeadsCrateAdapter::open(beads_dir, AdapterConfig::default()).await`. Adjust the trait object boxing — both implement `IssueTracker`, so the `Arc<dyn IssueTracker>` storage shouldn't change.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm`
Expected: tests pass.

```bash
git add crates/spur-pm/src/service.rs
git commit -m "spur-pm: PmService uses BeadsCrateAdapter (replaces shellout)"
```

---

### Task 27: Delete `crates/spur-pm/src/beads.rs`

**Files:**
- Delete: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/src/lib.rs`

- [ ] **Step 1: Move `PollCursor` if not yet moved**

Confirm `PollCursor` was moved to `poll_cursor.rs` in Task 20. If anything in `beads.rs` is still in use elsewhere, surface it before deletion.

- [ ] **Step 2: Delete the file**

```bash
rm crates/spur-pm/src/beads.rs
```

- [ ] **Step 3: Remove `pub mod beads;` from lib.rs**

- [ ] **Step 4: Build + run all tests**

Run: `cargo test --workspace`
Expected: clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/
git commit -m "spur-pm: delete BeadsAdapter shellout (superseded by BeadsCrateAdapter)"
```

---

### Task 28: Verify `Command::new("br")` is gone from production code

**Files:** none modified

- [ ] **Step 1: Grep**

Run: grep for `Command::new\("br"\)` across `crates/`.

- [ ] **Step 2: Decide on remaining occurrences**

If the only remaining hits are in `#[cfg(test)]` modules or under `tests/`, that's an acceptable end state for "no production br". If you want zero matches anywhere, also migrate those (covered in earlier tasks but worth confirming).

Document the result in a comment in the spec or in the PR description.

- [ ] **Step 3: Commit (if doc updates needed)**

---

### Task 29: Multi-process integration tests

**Files:**
- Create: `crates/spur-pm/tests/beads_crate_multiprocess.rs`

- [ ] **Step 1: Write concurrent-write stress test**

```rust
//! Multi-process tests for BeadsCrateAdapter — simulate multiple SPUR
//! instances against the same .beads/ directory.

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::test_workspace::TestBeadsWorkspace;

#[tokio::test]
async fn concurrent_writes_no_corruption() {
    let dir = TempDir::new().unwrap();
    // Initialize the workspace once
    {
        let _w = beads_rust::storage::sqlite::SqliteStorage::open(dir.path()).unwrap();
    }
    // Spawn 4 "instances" within the same process (each opens its own adapter,
    // so they share .beads/.write.lock for cross-process serialization).
    let mut handles = Vec::new();
    for instance_id in 0..4 {
        let path = dir.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            let adapter = BeadsCrateAdapter::open(&path, AdapterConfig::default()).await.unwrap();
            for i in 0..25 {
                use beads_rust::model::Issue;
                adapter.write(move |s| {
                    let issue = Issue::new(format!("inst{instance_id}-i{i}"));
                    s.create_issue(&issue, "test").map_err(anyhow::Error::from)
                }).await.unwrap();
            }
        }));
    }
    for h in handles { h.await.unwrap(); }
    // Verify total count
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let summaries = adapter.list_issues(Default::default()).await.unwrap();
    assert_eq!(summaries.len(), 100);
}
```

- [ ] **Step 2: Migration race test**

```rust
#[tokio::test]
async fn concurrent_first_open_serializes_via_migration_lock() {
    let dir = TempDir::new().unwrap();
    let h1 = tokio::spawn({
        let path = dir.path().to_path_buf();
        async move { BeadsCrateAdapter::open(&path, AdapterConfig::default()).await }
    });
    let h2 = tokio::spawn({
        let path = dir.path().to_path_buf();
        async move { BeadsCrateAdapter::open(&path, AdapterConfig::default()).await }
    });
    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();
    assert!(r1.is_ok() && r2.is_ok(), "both opens succeed (one waits, one wins migration)");
}
```

- [ ] **Step 3: Snapshot-conflict test**

```rust
#[tokio::test]
async fn snapshot_conflict_detected() {
    let dir = TempDir::new().unwrap();
    let adapter1 = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let adapter2 = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    use beads_rust::model::Issue;
    let id_a = adapter1.write(|s| {
        let issue = Issue::new("seed".to_string());
        let id = issue.id.clone();
        s.create_issue(&issue, "test").map_err(anyhow::Error::from)?;
        Ok(id)
    }).await.unwrap();
    let snap = adapter1.read_snapshot(|_s| Ok(())).await.unwrap();
    // adapter2 mutates between snapshot and commit
    adapter2.write(|s| {
        let mut update = beads_rust::storage::sqlite::IssueUpdate::default();
        update.status = Some(beads_rust::model::Status::Closed);
        s.update_issue(&id_a, &update, "test").map_err(anyhow::Error::from)
    }).await.unwrap();
    let result = adapter1.validate_and_commit(snap, |s, _| {
        s.create_issue(&Issue::new("after".to_string()), "test").map_err(anyhow::Error::from)
    }).await;
    assert!(result.is_err(), "expected Conflict; got {:?}", result);
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p spur-pm --test beads_crate_multiprocess`
Expected: 3 pass.

```bash
git add crates/spur-pm/tests/beads_crate_multiprocess.rs
git commit -m "spur-pm: multi-process integration tests for BeadsCrateAdapter"
```

---

### Task 30: Idempotent under-lock auto-flush + periodic task

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add `auto_flush()` method**

In `impl BeadsCrateAdapter`, add:

```rust
/// Idempotent under-lock auto-flush. Inside `.write.lock`, call
/// `beads_rust::sync::auto_flush` which is a no-op if nothing's dirty.
/// No leader election; multiple processes calling this concurrently is
/// safe — they serialize via the file lock and skip if not dirty.
pub async fn auto_flush(&self) -> anyhow::Result<()> {
    let writer = Arc::clone(&self.writer);
    let metrics = Arc::clone(&self.metrics);
    let beads_dir = self.beads_dir.clone();
    let backoff = self.config.backoff.clone();
    let lock_timeout_ms = self.config.lock_timeout_ms;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _flock = acquire_write_lock_with_backoff(&beads_dir, &backoff, lock_timeout_ms, &metrics)?;
        let mut writer_guard = writer.blocking_lock();
        let jsonl = beads_dir.join("issues.jsonl");
        let outcome = beads_rust::sync::auto_flush(
            &mut writer_guard,
            &jsonl,
            &Default::default(),
            &Default::default(),
        )?;
        // Inspect outcome; record metric. Adjust the match arms to whatever
        // the crate's AutoFlushOutcome enum returns.
        let _ = outcome;
        metrics.incr_auto_flush_success();
        Ok(())
    }).await?
}

/// Spawn a background task that calls auto_flush every `interval`.
/// Returns a JoinHandle for shutdown.
pub fn spawn_periodic_auto_flush(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = self.auto_flush().await {
                tracing::warn!(error = %e, "periodic auto_flush failed");
            }
        }
    })
}
```

- [ ] **Step 2: Test**

```rust
#[tokio::test]
async fn auto_flush_idempotent_when_clean() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    adapter.auto_flush().await.unwrap();
    adapter.auto_flush().await.unwrap(); // safe to call repeatedly
    assert!(adapter.metrics().auto_flush_success_total.load(std::sync::atomic::Ordering::Relaxed) >= 2);
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate::adapter --lib auto_flush`
Expected: pass.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: idempotent under-lock auto_flush + periodic task"
```

---

### Task 31: Tracing spans on every primitive

**Files:**
- Modify: `crates/spur-pm/src/beads_crate/adapter.rs`

- [ ] **Step 1: Add tracing spans to each primitive**

For each of `read`, `write`, `batch`, `read_snapshot`, `validate_and_commit`, `auto_flush`, wrap the body in a `tracing::info_span!` so operators can trace adapter activity.

Example for `read`:

```rust
pub async fn read<T, F>(&self, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&SqliteStorage) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let _span = tracing::info_span!("beads.read").entered();
    drop(_span); // sync span doesn't cross spawn_blocking; record duration via metric
    let start = std::time::Instant::now();
    let pool = Arc::clone(&self.reader_pool);
    let metrics = Arc::clone(&self.metrics);
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
        metrics.incr_read();
        let guard = pool.checkout()?;
        f(guard.storage())
    }).await?;
    tracing::trace!(duration_us = start.elapsed().as_micros() as u64, "beads.read.complete");
    result
}
```

Apply the same pattern to `write`, `batch`, `read_snapshot`, `validate_and_commit`, `auto_flush`. Keep span names lowercase-dotted (`beads.write`, etc.) for log filterability.

- [ ] **Step 2: Add a smoke test capturing a span**

```rust
#[tokio::test]
async fn read_emits_a_tracing_event() {
    use tracing_subscriber::fmt::TestWriter;
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("trace"))
        .with_writer(TestWriter::new())
        .try_init();
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default()).await.unwrap();
    let _ = adapter.read(|_s| Ok(())).await.unwrap();
    // The assertion is implicit: trace output appears in test logs (run with -- --nocapture).
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-pm beads_crate --lib`
Expected: existing tests pass; the new test runs without panic.

```bash
git add crates/spur-pm/src/beads_crate/adapter.rs
git commit -m "spur-pm: tracing spans on every adapter primitive"
```

---

### Task 32: Audit SPUR-internal single-writer assumptions

**Files:**
- Read-only sweep across `crates/spur-mcp/src/plan/*.rs` and `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Enumerate caches and snapshots**

Run grep tool for: `cache`, `Snapshot`, `last_seen`, `tick_state`, `lineage` across `crates/spur-mcp/src/`.

Document each match with: file path, what it caches, lifetime, whether it crosses `.await`, whether multi-instance writes could invalidate it.

- [ ] **Step 2: Decide per finding**

For each cache that could go stale due to another SPUR instance writing:
- **Option A**: shorten its lifetime to within one `spawn_blocking` closure (eliminates the staleness window).
- **Option B**: add `data_version` polling to detect invalidation.
- **Option C**: document that the cache is single-instance-only and add a runtime guard if multi-instance is detected.

For each finding, write the chosen mitigation as a follow-up beads issue (small change) or apply inline if trivial.

- [ ] **Step 3: Document findings + commit**

Create `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md` Open Questions section update OR a separate `docs/superpowers/notes/single-writer-audit.md` summarizing the audit results.

```bash
git add docs/
git commit -m "docs: single-writer-assumption audit notes for beads_rust crate adapter"
```

---

### Task 33: Final workspace test + documentation update

**Files:**
- Modify: `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md` (status → Implemented)

- [ ] **Step 1: Run the entire workspace test suite**

Run: `cargo test --workspace --all-features`
Expected: green.

Run: `cargo build --workspace --release`
Expected: clean build.

- [ ] **Step 2: Update spec status**

In the spec table at the top, change `Status: Draft` to `Status: Implemented`. Add a footer: "Implemented in PR #<n> on <date>; companion plan at `docs/superpowers/plans/2026-05-05-beads_rust-direct-crate-adapter.md`."

- [ ] **Step 3: Final commit**

```bash
git add docs/
git commit -m "docs(spec): mark beads_rust direct crate adapter as Implemented"
```

---

## Done

After all 30 tasks: `BeadsCrateAdapter` replaces `BeadsAdapter` in production; all tests pass against the new adapter; `Command::new("br")` is gone from production code (and ideally from tests too); multi-process correctness is exercised in CI; observability metrics are in place; the spec is marked Implemented.

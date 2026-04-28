# Log Rotation, Size Caps, and Filtering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound the on-disk and in-RAM cost of spur's `spur.log`, ACP child stderr, and `.spur/events/` streams; eliminate the deleted-FD-leak class of bugs that produced a 37 GB log inode.

**Architecture:** Compose `tracing_appender::non_blocking::lossy(true)` (outermost) with `file-rotate 0.8` (inner sink) per spur.log + per-child stderr. Add `EnvFilter` to TUI tracing init. Downgrade three high-frequency `info!` callsites to `debug!`. Add `max_files` GC to the existing `event_sink.rs`. Configurable via a new `[log]` section in `SpurConfig`. Date-aware basepath wrapper preserves the `spur.log.YYYY-MM-DD*` runbook glob pattern. Per-child `buffered_lines_limit(8192)` bounds in-RAM. Bounded byte-chunk reads on child stderr defend against newline-less output.

**Tech Stack:** Rust 1.88 / edition 2024, Tokio, `tracing` + `tracing-subscriber` + `tracing-appender 0.2.4` (existing), new dep `file-rotate 0.8`, `serde` for config, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-04-28-log-rotation-design.md`

**Sibling plan:** `docs/superpowers/plans/2026-04-28-orphan-reaping.md`

**Followup tickets:** `bd-3qt` (medium/low amendments — scope-reduced after file-rotate), `bd-1km` (DuckDB cost cache GC).

---

## File Map

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-cli/Cargo.toml` | Modify | Add `file-rotate = "0.8"` dependency |
| `crates/spur-acp/src/config/mod.rs` | Modify | Add `[log]` section to `SpurConfig` |
| `crates/spur-cli/src/main.rs` | Modify | Add `EnvFilter`, swap `rolling::daily` → file-rotate composition |
| `crates/spur-cli/src/log_writer.rs` | Create | Date-aware basepath helper, `WorkerGuard` lifecycle |
| `crates/spur-cli/src/lib.rs` | Modify | Re-export `init_tracing` for test seam |
| `crates/spur-cli/tests/log_rotation.rs` | Create | Integration: 100 MB → ≤ 32 MB on disk + gzip rotated chunks |
| `crates/spur-acp/src/connection/native.rs` | Modify | Replace `Stdio::from(file)` with `Stdio::piped()` + bridge task |
| `crates/spur-acp/src/connection/child_stderr_bridge.rs` | Create | Per-child file-rotate writer + bounded byte-chunk reader |
| `crates/spur-acp/tests/child_stderr_piping.rs` | Create | Integration: 50 MB stderr burst, `\r`-only OOM defense |
| `crates/spur-core/src/orchestrator.rs:2624-2632` | Modify | Downgrade per-tool/per-chunk debug |
| `crates/spur-acp/src/connection/native.rs:1330-1338` | Modify | Downgrade per-ACP-notification debug |
| `crates/spur-tui/src/app.rs:2798-2804` | Modify | Downgrade per-render debug |
| `crates/spur-core/src/event_sink.rs` | Modify | Add `enforce_event_cap` GC after rotation |
| `crates/spur-core/src/event_sink.rs` (test mod) | Modify | Add GC test |

---

## Task 1: Add `[log]` config section to `SpurConfig`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (struct around line 348)
- Test: `crates/spur-acp/src/config/mod.rs` (existing test mod)

- [ ] **Step 1: Write the failing test**

In the existing test module of `crates/spur-acp/src/config/mod.rs`, add:

```rust
#[test]
fn parses_log_section_with_defaults() {
    let toml_str = r#"
[log]
level = "warn,spur_core::orchestrator=info"
"#;
    let cfg: SpurConfig = toml::from_str(toml_str).expect("valid config");
    assert_eq!(cfg.log.level, "warn,spur_core::orchestrator=info");
    assert_eq!(cfg.log.max_file_bytes, 8_388_608);   // 8 MB
    assert_eq!(cfg.log.max_files, 3);
    assert_eq!(cfg.log.buffered_lines_limit, 8_192);
    assert_eq!(cfg.log.child_stderr_max_bytes, 2_621_440); // 2.5 MB
    assert_eq!(cfg.log.child_stderr_max_files, 3);
    assert_eq!(cfg.log.events_max_total_bytes, 67_108_864); // 64 MB
    assert!(cfg.log.child_stderr_pipe);
}

#[test]
fn parses_empty_config_uses_log_defaults() {
    let cfg: SpurConfig = toml::from_str("").expect("empty config valid");
    assert_eq!(cfg.log.level, "warn,spur_core::orchestrator=info");
    assert_eq!(cfg.log.max_file_bytes, 8_388_608);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib config::tests::parses_log_section_with_defaults`
Expected: FAIL — `LogConfig` not defined; `SpurConfig` has no `log` field.

- [ ] **Step 3: Define `LogConfig` and add `log: LogConfig` to `SpurConfig`**

In `crates/spur-acp/src/config/mod.rs`, near the other config structs:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing-subscriber` `EnvFilter` directives. `RUST_LOG` overrides if set.
    pub level: String,

    /// Per-chunk byte limit for `spur.log.YYYY-MM-DD.<n>`.
    pub max_file_bytes: u64,

    /// Number of rotated chunks to keep (`MaxFiles` argument to `file-rotate`).
    /// Active file + max_files = total chunks; total bytes ≤ (max_files+1) × max_file_bytes.
    pub max_files: usize,

    /// `tracing_appender::non_blocking` channel depth.
    pub buffered_lines_limit: usize,

    /// Per-chunk byte limit for ACP child stderr files.
    pub child_stderr_max_bytes: u64,

    /// Number of rotated chunks per child to keep.
    pub child_stderr_max_files: usize,

    /// Total byte cap for `.spur/events/` ndjson directory.
    pub events_max_total_bytes: u64,

    /// If false, fall back to direct-FD stderr capture (legacy model, no rotation).
    pub child_stderr_pipe: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "warn,spur_core::orchestrator=info".to_string(),
            max_file_bytes: 8_388_608,
            max_files: 3,
            buffered_lines_limit: 8_192,
            child_stderr_max_bytes: 2_621_440,
            child_stderr_max_files: 3,
            events_max_total_bytes: 67_108_864,
            child_stderr_pipe: true,
        }
    }
}
```

Add to `SpurConfig`:

```rust
#[serde(default)]
pub log: LogConfig,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-acp --lib config::`
Expected: PASS — both new tests + existing config tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): add [log] section to SpurConfig with defaults"
```

---

## Task 2: Add `EnvFilter` to TUI-mode `init_tracing`

**Files:**
- Modify: `crates/spur-cli/src/main.rs:20-55`

This is the same-day "stop the bleeding" fix. We add the EnvFilter even before `file-rotate` is wired in — once this lands, the 650 KB/sec firehose drops to ≲ 5 KB/sec.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-cli/tests/env_filter_smoke.rs`:

```rust
//! Smoke test: TUI-mode init_tracing must accept an EnvFilter so DEBUG events
//! are filtered. Until Task 4, this test only exercises the filter, not rotation.

use std::process::Command;
use tempfile::tempdir;

#[test]
fn tui_mode_filters_debug_events_by_default() {
    let dir = tempdir().expect("tmpdir");
    // Spawn `spur tui --help` with the tempdir as CWD; --help short-circuits
    // before the TUI starts but after init_tracing has run.
    // We assert that .spur/logs is created without DEBUG noise.
    let output = Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["tui", "--help"])
        .current_dir(dir.path())
        .output()
        .expect("spawn spur");
    assert!(output.status.success(), "spur tui --help failed: {output:?}");
    // Note: this is an early smoke. Full byte-cap verification lands in Task 4.
}
```

- [ ] **Step 2: Run test to verify it fails or builds**

Run: `cargo test -p spur-cli --test env_filter_smoke`
Expected: FAIL or NOT FOUND — `init_tracing` may not have an EnvFilter yet; if it does, this is just a smoke. If the test scaffolding is missing (no `CARGO_BIN_EXE_spur` link), document and proceed; this is a pre-implementation smoke.

- [ ] **Step 3: Add `EnvFilter` to TUI-mode init**

In `crates/spur-cli/src/main.rs`, replace lines 26-34 (the TUI branch):

```rust
if tui_mode {
    let log_dir = repo_root.join(".spur").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    // Read [log].level from config; fall back to default. RUST_LOG overrides.
    let cfg_level = "warn,spur_core::orchestrator=info"; // TODO Task 4: read from SpurConfig
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cfg_level));

    let file_appender = tracing_appender::rolling::daily(&log_dir, "spur.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();
    Ok(Some(guard))
}
```

(The `cfg_level` literal becomes `cfg.log.level` after Task 4 wires config into `init_tracing`. Marker comment so we don't lose the threading.)

- [ ] **Step 4: Run smoke test**

Run: `cargo test -p spur-cli --test env_filter_smoke`
Expected: PASS — smoke succeeds; `spur tui --help` exits 0.

- [ ] **Step 5: Manual verification of disk impact**

Run a one-shot manual check (copy into a notes block, not a test):

```bash
# Before EnvFilter (control): not applicable post-fix, skip.
# After: launch spur tui briefly, ensure spur.log.<TODAY> grows at < 50 KB/sec
RUST_LOG="" cargo run -p spur-cli -- tui --brain claude-code &
sleep 60; kill %1
ls -lh .spur/logs/spur.log.*
# Expected: < 5 MB after 60 seconds (was ~40 MB before EnvFilter at 650 KB/sec)
```

Document the observed rate in the commit message.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/main.rs crates/spur-cli/tests/env_filter_smoke.rs
git commit -m "fix(spur-cli): add EnvFilter to TUI-mode init_tracing

Stops the 650 KB/sec firehose that produced the 37 GB spur.log incident.
Default 'warn,spur_core::orchestrator=info'; RUST_LOG overrides.

Same-day stop-gap; size-based rotation lands in Task 4.
"
```

---

## Task 3: Downgrade three high-frequency `info!` callsites to `debug!`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:2624-2632`
- Modify: `crates/spur-acp/src/connection/native.rs:1330-1338`
- Modify: `crates/spur-tui/src/app.rs:2798-2804`

These are the per-tool, per-ACP-notification, and per-render callsites the gate-1 review identified as the actual high-volume sources. They were `info!` (above the new `EnvFilter` default).

- [ ] **Step 1: Read current state to confirm line numbers**

Run:
```bash
grep -n "info!" crates/spur-core/src/orchestrator.rs | head -50
grep -n "info!" crates/spur-acp/src/connection/native.rs | head -50
grep -n "info!" crates/spur-tui/src/app.rs | head -30
```

Confirm the per-tool / per-notification / per-render callsites at the cited line ranges. Lines may have drifted; cite final lines in the commit.

- [ ] **Step 2: Downgrade `orchestrator.rs:2624-2632` (per-tool/per-chunk)**

Change the `info!(...)` macro at the per-tool callsite to `debug!(...)`. Preserve all fields. Same syntactic form, just the macro name differs.

- [ ] **Step 3: Downgrade `native.rs:1330-1338` (per-ACP-notification)**

Same change. `info!` → `debug!`.

- [ ] **Step 4: Downgrade `app.rs:2798-2804` (per-render)**

Same change. `info!` → `debug!`.

- [ ] **Step 5: Run the workspace test suite**

Run: `cargo test --workspace --lib`
Expected: PASS — these are formatting changes; no test should depend on the level.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs \
        crates/spur-acp/src/connection/native.rs \
        crates/spur-tui/src/app.rs
git commit -m "perf(logging): downgrade 3 hot info! callsites to debug!

Per gate-1 review of the 37 GB spur.log incident, these are the actual
high-frequency sources:
- orchestrator.rs:<final line> per-tool/per-chunk
- native.rs:<final line> per-ACP-notification
- app.rs:<final line> per-render

With EnvFilter at 'warn,spur_core::orchestrator=info' default, these now
require RUST_LOG=debug to surface.
"
```

---

## Task 4: Add `file-rotate` dependency and `log_writer` module

**Files:**
- Modify: `crates/spur-cli/Cargo.toml`
- Create: `crates/spur-cli/src/log_writer.rs`
- Modify: `crates/spur-cli/src/lib.rs` (re-export `init_tracing` for tests)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-cli/tests/log_rotation.rs`:

```rust
//! Integration: 100 MB of tracing output, assert ≤ 32 MB + 64 KB on disk
//! and that exactly 4 chunks exist (1 active + 3 rotated).

use spur_cli::init_tracing_for_test;
use std::time::Duration;
use tempfile::tempdir;
use tracing::info;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotates_at_8mb_keeps_4_chunks() {
    let dir = tempdir().expect("tmpdir");
    let _guard = init_tracing_for_test(dir.path()).expect("init");

    let payload = "x".repeat(8_000); // 8 KB per event
    for _ in 0..14_000 {
        info!(target: "spur_core::orchestrator", payload = %payload, "emit");
    }
    drop(_guard);
    // Allow worker thread to flush and gzip rotations.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let logs_dir = dir.path().join(".spur/logs");
    let mut total_bytes = 0u64;
    let mut chunks = vec![];
    for entry in std::fs::read_dir(&logs_dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("spur.log.") {
            let len = entry.metadata().expect("md").len();
            total_bytes += len;
            chunks.push((name, len));
        }
    }

    assert!(total_bytes <= 32 * 1_024 * 1_024 + 64 * 1_024,
        "total bytes {} exceeds 32 MB + 64 KB slop", total_bytes);
    assert_eq!(chunks.len(), 4,
        "expected 4 chunks (1 active + 3 rotated), got {} = {:?}",
        chunks.len(), chunks);

    // At least one rotated chunk should be gzipped.
    let gz_count = chunks.iter().filter(|(n, _)| n.ends_with(".gz")).count();
    assert!(gz_count >= 1, "expected ≥ 1 .gz rotated chunk, got {}", gz_count);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-cli --test log_rotation`
Expected: FAIL — `init_tracing_for_test` not exported; `file-rotate` not in deps.

- [ ] **Step 3: Add the dependency**

Edit `crates/spur-cli/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
file-rotate = "0.8"
```

Run `cargo update -p file-rotate` to lock the version (`Cargo.lock` will gain `file-rotate 0.8.x` + `flate2` + `compress` chain).

- [ ] **Step 4: Create `crates/spur-cli/src/log_writer.rs`**

```rust
//! Date-aware basepath helper for spur.log rotation.
//!
//! `file-rotate` puts the active file at `<basepath>` and rotated chunks
//! at `<basepath>.0[.gz]`, `<basepath>.1[.gz]`, etc. We choose the basepath
//! per-session as `spur.log.YYYY-MM-DD` so the active file matches the
//! existing `spur.log.YYYY-MM-DD*` runbook glob, and rotated chunks
//! become `spur.log.YYYY-MM-DD.0.gz`, `.1.gz`, etc.
//!
//! Mid-session date rollover is out of scope (sessions are short relative
//! to a day; SIGUSR1 rebuild is a future enhancement).

use chrono::Utc;
use file_rotate::{
    compression::Compression,
    suffix::{AppendCount, FileLimit},
    ContentLimit, FileRotate,
};
use std::path::{Path, PathBuf};

/// Compute today's basepath under the given log dir.
pub fn today_basepath(log_dir: &Path) -> PathBuf {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    log_dir.join(format!("spur.log.{today}"))
}

/// Build the `FileRotate` instance configured per `[log]` section defaults.
pub fn build_rotator(
    log_dir: &Path,
    max_file_bytes: u64,
    max_files: usize,
) -> FileRotate<AppendCount> {
    let basepath = today_basepath(log_dir);
    FileRotate::new(
        basepath,
        AppendCount::new(max_files),
        ContentLimit::Bytes(max_file_bytes as usize),
        Compression::OnRotate(0),
        #[cfg(unix)]
        Some(0o600),
        #[cfg(not(unix))]
        None,
    )
}
```

- [ ] **Step 5: Wire `init_tracing` to use the rotator**

In `crates/spur-cli/src/main.rs`, replace the TUI branch (Task 2's interim version):

```rust
mod log_writer;

if tui_mode {
    let log_dir = repo_root.join(".spur").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    // Load SpurConfig to get [log] settings (keeps default if no config).
    let spur_config = spur_acp::config::SpurConfig::load(repo_root)
        .unwrap_or_default();
    let log_cfg = &spur_config.log;

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&log_cfg.level));

    let rotator = log_writer::build_rotator(
        &log_dir,
        log_cfg.max_file_bytes,
        log_cfg.max_files,
    );
    let (non_blocking, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(log_cfg.buffered_lines_limit)
        .finish(rotator);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();
    Ok(Some(guard))
}
```

- [ ] **Step 6: Add the test seam**

Create or modify `crates/spur-cli/src/lib.rs`:

```rust
//! Library re-exports for spur-cli internals (test seam).
mod log_writer;

pub use log_writer::{today_basepath, build_rotator};

/// Test seam: equivalent to TUI-mode `init_tracing`, but takes an explicit
/// repo root and returns the `WorkerGuard` so tests can drop it before
/// asserting on disk state.
#[cfg(any(test, feature = "test-seam"))]
pub fn init_tracing_for_test(
    repo_root: &std::path::Path,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let log_dir = repo_root.join(".spur").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let env_filter = tracing_subscriber::EnvFilter::new(
        "warn,spur_core::orchestrator=info"
    );

    let rotator = log_writer::build_rotator(&log_dir, 8_388_608, 3);
    let (non_blocking, guard) =
        tracing_appender::non_blocking::NonBlockingBuilder::default()
            .lossy(true)
            .buffered_lines_limit(8_192)
            .finish(rotator);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();

    Ok(guard)
}
```

If `lib.rs` already exists, just append the `init_tracing_for_test` function and the `mod log_writer; pub use ...` line.

- [ ] **Step 7: Update `Cargo.toml` for the test seam**

In `crates/spur-cli/Cargo.toml`, add the feature so the function is accessible from the integration test:

```toml
[features]
test-seam = []
```

In `[dev-dependencies]` ensure `tempfile`, `tokio` (with `rt-multi-thread`, `macros`, `time`), `tracing` are present (most likely already are; verify).

- [ ] **Step 8: Run integration test**

Run: `cargo test -p spur-cli --test log_rotation --features test-seam`
Expected: PASS — total bytes ≤ 32 MB + 64 KB; 4 chunks present; ≥ 1 `.gz` rotated chunk.

- [ ] **Step 9: Run full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS — no regressions.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-cli/Cargo.toml \
        crates/spur-cli/src/main.rs \
        crates/spur-cli/src/log_writer.rs \
        crates/spur-cli/src/lib.rs \
        crates/spur-cli/tests/log_rotation.rs \
        Cargo.lock
git commit -m "feat(spur-cli): adopt file-rotate + non_blocking::lossy for spur.log

- Add file-rotate 0.8 dependency.
- Date-aware basepath: spur.log.YYYY-MM-DD active, .0.gz/.1.gz/... rotated.
- non_blocking::lossy(true) + buffered_lines_limit(8192) caps in-RAM.
- Compression::OnRotate(0) gzips rotated chunks (~8x shrinkage typical).
- Total disk: 4 chunks × 8 MB = 32 MB cap; ~12 MB on disk with gzip.
- New test seam (lib.rs + test-seam feature) for the integration test.

Closes the L1/L2/L4 high-severity bugs from the 6-gate spec review:
file-rotate runs on the non_blocking worker thread (no hot-path mutex,
no application-thread blocking), and closes the active file before any
rename/unlink (no deleted-FD recurrence).
"
```

---

## Task 5: Pipe ACP child stderr through per-child file-rotate

**Files:**
- Create: `crates/spur-acp/src/connection/child_stderr_bridge.rs`
- Modify: `crates/spur-acp/src/connection/native.rs:877-906`
- Modify: `crates/spur-acp/src/connection/mod.rs` (declare new module)
- Test: `crates/spur-acp/tests/child_stderr_piping.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/child_stderr_piping.rs`:

```rust
//! Integration: spawn a process that prints 50 MB to stderr in 10 MB bursts;
//! assert per-child file usage stays ≤ 10 MB. Then run a `\r`-only burst and
//! assert spur task does not OOM (process memory stays under 100 MB).

use spur_acp::connection::child_stderr_bridge::ChildStderrBridge;
use std::process::Stdio;
use tempfile::tempdir;
use tokio::process::Command;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_mb_stderr_burst_capped_at_ten_mb() {
    let dir = tempdir().expect("tmpdir");
    let log_path = dir.path().join("test-agent.log");

    // /bin/sh script that prints ~50 MB to stderr in 10 MB bursts.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"for i in 1 2 3 4 5; do head -c 10485760 < /dev/urandom | base64 1>&2; done"#)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let stderr = child.stderr.take().expect("stderr piped");
    let bridge = ChildStderrBridge::start(
        stderr,
        log_path.parent().unwrap(),
        "test-agent",
        child.id().expect("pid"),
        2_500_000,  // 2.5 MB per chunk
        3,          // 3 rotated + 1 active = 10 MB total
        8_192,      // buffered_lines_limit
    ).expect("start bridge");

    let _ = child.wait().await;
    bridge.shutdown().await;

    // Sum all test-agent-*.log* sizes.
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir.path()).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("test-agent") {
            total += entry.metadata().expect("md").len();
        }
    }
    assert!(total <= 10 * 1_024 * 1_024 + 64 * 1_024,
        "child stderr total {} exceeds 10 MB + slop", total);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newline_less_burst_does_not_oom() {
    let dir = tempdir().expect("tmpdir");
    let log_path = dir.path().join("rprog-agent.log");

    // Print 5 MB of `\r`-prefixed progress without a single newline.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"head -c 5242880 < /dev/zero | tr '\0' '\r' 1>&2"#)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let stderr = child.stderr.take().expect("stderr piped");
    let bridge = ChildStderrBridge::start(
        stderr,
        log_path.parent().unwrap(),
        "rprog-agent",
        child.id().expect("pid"),
        2_500_000, 3, 8_192,
    ).expect("start bridge");

    // Test passes if it completes within the timeout (no infinite buffer growth).
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        async {
            let _ = child.wait().await;
            bridge.shutdown().await;
        },
    ).await;
    assert!(result.is_ok(), "newline-less stderr burst hung");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test child_stderr_piping`
Expected: FAIL — `ChildStderrBridge` not defined.

- [ ] **Step 3: Create `crates/spur-acp/src/connection/child_stderr_bridge.rs`**

```rust
//! Per-child stderr bridge: bounded byte-chunk reader → file-rotate writer.
//!
//! Replaces the legacy `Stdio::from(File)` approach where the child held the
//! FD. Now spur owns the writer (so `rm` cannot recreate the deleted-FD
//! pattern) and applies per-child size caps.
//!
//! Key choices, all anchored in the gate-2 review:
//! - **Bounded byte-chunk reads**, not `read_line`. Defends against
//!   `\r`-only progress bars that never deliver a newline.
//! - `non_blocking::lossy(true)` for backpressure: drop on full, do not
//!   block the child's stderr write.
//! - Per-child `buffered_lines_limit` so N=8 children stay ≤ 32 MB in-RAM.

use anyhow::Result;
use file_rotate::{
    compression::Compression,
    suffix::AppendCount,
    ContentLimit, FileRotate,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

const READ_BUF_SIZE: usize = 16 * 1024;

pub struct ChildStderrBridge {
    /// Held until shutdown so the worker thread continues draining.
    _guard: WorkerGuard,
    /// Reader task; awaits child stderr EOF.
    reader: JoinHandle<()>,
    /// Per-child dropped-bytes counter (incremented on `try_send` Err(Full)).
    dropped_bytes: Arc<AtomicU64>,
    /// Agent name for log lines.
    agent_name: String,
    /// Child pid for log lines.
    pid: u32,
}

impl ChildStderrBridge {
    /// Spawn a reader task that pipes the child's stderr into a per-child
    /// `FileRotate` writer behind a `non_blocking` worker.
    pub fn start<R>(
        stderr: R,
        log_dir: &Path,
        agent_name: &str,
        pid: u32,
        max_file_bytes: u64,
        max_files: usize,
        buffered_lines_limit: usize,
    ) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let basepath = log_dir.join(format!(
            "{agent_name}-{pid}.log",
        ));
        let rotator = FileRotate::new(
            basepath,
            AppendCount::new(max_files),
            ContentLimit::Bytes(max_file_bytes as usize),
            Compression::OnRotate(0),
            #[cfg(unix)]
            Some(0o600),
            #[cfg(not(unix))]
            None,
        );
        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(true)
            .buffered_lines_limit(buffered_lines_limit)
            .finish(rotator);

        let dropped_bytes = Arc::new(AtomicU64::new(0));
        let dropped = dropped_bytes.clone();
        let agent = agent_name.to_string();

        let reader = tokio::spawn(reader_loop(stderr, writer, dropped, agent.clone(), pid));

        Ok(Self {
            _guard: guard,
            reader,
            dropped_bytes,
            agent_name: agent,
            pid,
        })
    }

    /// Block until reader EOFs (child stderr closed) and emit a single
    /// `child_stderr_lagging` summary if any bytes were dropped.
    pub async fn shutdown(self) {
        let _ = self.reader.await;
        let dropped = self.dropped_bytes.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::error!(
                agent = %self.agent_name,
                pid = self.pid,
                dropped_bytes = dropped,
                "child_stderr_lagging: bridge dropped bytes due to backpressure"
            );
        }
        // _guard drops here, draining the worker.
    }
}

async fn reader_loop<R>(
    mut stderr: R,
    mut writer: NonBlocking,
    dropped_bytes: Arc<AtomicU64>,
    agent: String,
    pid: u32,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    use std::io::Write;
    let mut buf = [0u8; READ_BUF_SIZE];
    let mut first_drop_logged = false;
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                // try_send semantics ride on `non_blocking::lossy(true)`:
                // a full channel returns Err(io::Error) which we count.
                if writer.write_all(&buf[..n]).is_err() {
                    dropped_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if !first_drop_logged {
                        first_drop_logged = true;
                        tracing::error!(
                            agent = %agent,
                            pid = pid,
                            "child_stderr_lagging: bounded channel full; dropping bytes"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    agent = %agent, pid = pid, error = %e,
                    "child_stderr_bridge: read error; ending bridge"
                );
                break;
            }
        }
    }
}
```

- [ ] **Step 4: Declare module in `connection/mod.rs`**

In `crates/spur-acp/src/connection/mod.rs`, add:

```rust
pub mod child_stderr_bridge;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-acp --test child_stderr_piping`
Expected: PASS — both tests green.

- [ ] **Step 6: Wire into `native.rs:877-906`**

In `crates/spur-acp/src/connection/native.rs`, find the existing block:

```rust
let stderr_cfg = match std::fs::OpenOptions::new()
    .create(true).write(true).truncate(true)
    .open(&log_path)
{
    Ok(f) => std::process::Stdio::from(f),
    Err(e) => {
        tracing::warn!(...);
        std::process::Stdio::inherit()
    }
};
```

Replace with:

```rust
let stderr_cfg = if cfg.log.child_stderr_pipe {
    std::process::Stdio::piped()
} else {
    // Legacy fall-back path; same as today.
    match std::fs::OpenOptions::new()
        .create(true).write(true).truncate(true)
        .open(&log_path)
    {
        Ok(f) => std::process::Stdio::from(f),
        Err(e) => {
            tracing::warn!(
                agent = %agent_name,
                path = %log_path.display(),
                error = %e,
                "child_stderr_pipe disabled but log open failed; using inherit"
            );
            std::process::Stdio::inherit()
        }
    }
};
```

After the `cmd.spawn()` succeeds and you have `child` in scope, before the existing per-stream split, insert (gated on `cfg.log.child_stderr_pipe`):

```rust
let stderr_bridge_handle = if cfg.log.child_stderr_pipe {
    let stderr = child.stderr.take().expect("stderr should be piped");
    let log_dir = log_path.parent().expect("log path has parent").to_path_buf();
    Some(ChildStderrBridge::start(
        stderr,
        &log_dir,
        &agent_name,
        child.id().expect("child pid"),
        cfg.log.child_stderr_max_bytes,
        cfg.log.child_stderr_max_files,
        cfg.log.buffered_lines_limit,
    )?)
} else {
    None
};
```

Store `stderr_bridge_handle` in the connection's state so `shutdown()` can `bridge.shutdown().await` before the connection is dropped.

- [ ] **Step 7: Run the full ACP test suite**

Run: `cargo test -p spur-acp`
Expected: PASS — including existing `process_kill_on_drop.rs` (the bridge does not change kill semantics, only the stderr capture path).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/src/connection/child_stderr_bridge.rs \
        crates/spur-acp/src/connection/mod.rs \
        crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/tests/child_stderr_piping.rs
git commit -m "feat(spur-acp): pipe child stderr through file-rotate per agent

- Replace Stdio::from(File) with Stdio::piped() + bounded reader task.
- Per-child FileRotate: 4 chunks × 2.5 MB = 10 MB cap, gzipped rotations.
- non_blocking::lossy(true) + bounded byte-chunk reads (16 KB) defend
  against \\r-only newline-less output and pipe-full child stalls.
- Gated on [log] child_stderr_pipe = true; falls back to direct-FD model
  when disabled.
- Tests: 50 MB burst stays ≤ 10 MB; \\r-only burst does not OOM.
"
```

---

## Task 6: Add `enforce_event_cap` GC to `event_sink.rs`

**Files:**
- Modify: `crates/spur-core/src/event_sink.rs`
- Modify: `crates/spur-core/src/event_sink.rs` (test mod)

- [ ] **Step 1: Write the failing test**

In `crates/spur-core/src/event_sink.rs` test module, add:

```rust
#[tokio::test]
async fn enforces_max_total_bytes_after_rotation() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // Use a small max_total_bytes so we trigger GC quickly.
    let max_total: u64 = 256 * 1_024; // 256 KB total cap
    let max_per_file: u64 = 64 * 1_024; // 64 KB per file → ~4 files at cap

    let sink = SinkState::open_with_caps(
        dir.path(),
        max_per_file,
        max_total,
    ).expect("open");

    // Write enough events to trigger 6 rotations.
    for _ in 0..200 {
        sink.append(&fake_event_2_kb()).await.expect("append");
    }
    sink.flush_now().await;

    let mut total = 0u64;
    let mut count = 0;
    for entry in std::fs::read_dir(dir.path()).expect("read_dir") {
        let entry = entry.expect("entry");
        if entry.file_name().to_string_lossy().ends_with(".ndjson") {
            total += entry.metadata().expect("md").len();
            count += 1;
        }
    }
    assert!(total <= max_total, "total bytes {} exceeds cap {}", total, max_total);
    assert!(count >= 1, "expected at least 1 file, got {}", count);
}

fn fake_event_2_kb() -> SpurEvent {
    // Helper: produce a fake event whose JSONL representation is ≈ 2 KB.
    use spur_core::event::{SpurEventBody, SpurEvent};
    SpurEvent {
        seq: 0,
        body: SpurEventBody::AgentNotification {
            agent_name: "test".into(),
            text: "x".repeat(1900),
        },
        timestamp: chrono::Utc::now(),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib event_sink::tests::enforces_max_total_bytes_after_rotation`
Expected: FAIL — `SinkState::open_with_caps` does not exist; rotation has no GC.

- [ ] **Step 3: Add `enforce_event_cap` and `open_with_caps`**

In `crates/spur-core/src/event_sink.rs`:

```rust
/// Garbage-collect oldest `.ndjson` files in `dir` until total size ≤ cap.
/// Returns the number of files deleted (for telemetry).
fn enforce_event_cap(dir: &std::path::Path, cap_bytes: u64) -> std::io::Result<usize> {
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
            continue;
        }
        let md = entry.metadata()?;
        entries.push((path, md.modified()?, md.len()));
    }
    // Sort newest-first so we keep newest until we cross the cap.
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let mut running = 0u64;
    let mut deleted = 0usize;
    for (path, _mtime, size) in entries {
        running += size;
        if running > cap_bytes {
            std::fs::remove_file(&path)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}
```

Add a `open_with_caps` constructor that records `max_total_bytes` on `SinkState` and calls `enforce_event_cap` after every rotation.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --lib event_sink::tests::enforces_max_total_bytes_after_rotation`
Expected: PASS — total stays ≤ cap.

- [ ] **Step 5: Wire into `events_max_total_bytes` from config**

In the event-sink construction site (search for `SinkState::open` callers in `spur-core`), pass `cfg.log.events_max_total_bytes` (default 64 MB).

- [ ] **Step 6: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/event_sink.rs
git commit -m "feat(spur-core): enforce_event_cap GC for .spur/events/

After every rotation, delete oldest .ndjson files until total ≤
events_max_total_bytes (64 MB default per [log] config).

Closes the secondary disk-leak path: rotation worked, but old chunks
accumulated indefinitely (1.7 GB observed in the 37 GB incident).
"
```

---

## Task 7: Manual verification + acceptance check

**Files:**
- None modified; this is a verification ritual.

- [ ] **Step 1: Disk footprint sanity check**

```bash
# Clean slate
rm -rf .spur/logs .spur/events
# Run a 5-minute spur tui session with brain=claude-code emitting tool calls
cargo run --release -p spur-cli -- tui --brain claude-code &
sleep 300; kill %1
# Inspect
du -sh .spur/logs .spur/events
ls -lh .spur/logs/spur.log.*
ls -lh .spur/events/*.ndjson 2>/dev/null | wc -l
```

Expected:
- `.spur/logs/spur.log.<TODAY>` ≤ 8 MB
- `.spur/logs/spur.log.<TODAY>.0.gz` etc., total ≤ 32 MB
- `.spur/events/` ≤ 64 MB
- Per-agent `.spur/logs/<agent>-*.log*` totals ≤ 10 MB each

- [ ] **Step 2: `\r`-only stress (manual)**

Run a known-misbehaving stub agent that emits 10 MB of `\r`-prefixed progress without a newline. Observe spur memory in `htop` (or `Activity Monitor`). Confirm it stays bounded (< 100 MB resident).

- [ ] **Step 3: Mark acceptance criteria from spec**

Open `docs/superpowers/specs/2026-04-28-log-rotation-design.md` and tick the acceptance-criteria checkboxes that are now verifiable. Commit:

```bash
git add docs/superpowers/specs/2026-04-28-log-rotation-design.md
git commit -m "docs(spec): tick log-rotation acceptance criteria after Tasks 1-6"
```

---

## Self-review checklist (run before declaring done)

- [ ] All steps numbered and bite-sized.
- [ ] No "TBD", "implement later", "similar to Task N" placeholders.
- [ ] Every code step shows actual code.
- [ ] File paths are exact.
- [ ] Test commands have expected output.
- [ ] Each task ends with a commit step.
- [ ] Spec coverage: every section of `2026-04-28-log-rotation-design.md` maps to a task — `[log]` config = T1, EnvFilter = T2, downgrades = T3, file-rotate composition = T4, child stderr = T5, events GC = T6, manual verification = T7.

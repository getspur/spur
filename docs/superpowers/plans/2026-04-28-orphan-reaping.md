# Orphan ACP Tree Reaping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop accumulating orphan ACP agent process trees (PPID=1, ETIME up to 1d 18h) when spur is killed by SIGKILL/OOM/runtime-shutdown_timeout. Add a durable on-disk pgid registry plus an identity-verified startup sweep that reaps stale trees safely.

**Architecture:** Persist `<pgid>.toml` records to `.spur/pgids/` at every agent spawn; delete on graceful shutdown. On every spur startup, walk the directory: for each record, verify (owner spur dead) AND (recorded pgid leader's cmd + start-time still match) before issuing `killpg(SIGTERM)` → `killpg(SIGKILL)`. Replace `signal_hook` (which is not actually in the codebase — verified zero source refs) with `tokio::signal::unix` handlers for SIGTERM/SIGHUP/SIGQUIT routed through crossterm's event stream. Sweep runs unconditionally (no license gate).

**Tech Stack:** Rust 1.88 / edition 2024, Tokio (signal + spawn_blocking), `libproc` crate (macOS process inspection), `chrono` for timestamps, `serde` + `toml` for `.toml` records, `nix` for `kill(pid, 0)` / `getpgid`.

**Spec:** `docs/superpowers/specs/2026-04-28-orphan-reaping-design.md`

**Sibling plan:** `docs/superpowers/plans/2026-04-28-log-rotation.md`

**Followup tickets:** `bd-20k` (medium/low amendments — sequencing rewrite, schema_version, flock, TUI rendering arm, integration test scaffolding).

---

## File Map

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/connection/stdio_adapter.rs:107` | Modify | Add `cmd.process_group(0)` |
| `crates/spur-acp/src/connection/stream_json_adapter.rs:183` | Modify | Add `cmd.process_group(0)` |
| `crates/spur-acp/src/connection/cli_wrap_adapter.rs:188` | Modify | Add `cmd.process_group(0)` |
| `crates/spur-acp/src/connection/native.rs:1108` | Modify | Add `cmd.process_group(0)` (terminal/create) |
| `crates/spur-acp/src/orphan_registry.rs` | Create | `PgidRecord` struct + load/save/delete |
| `crates/spur-acp/src/connection/native.rs:907-928` | Modify | Write `.toml` after spawn |
| `crates/spur-acp/src/connection/native.rs:358-378` | Modify | Delete `.toml` after Drop's killpg |
| `crates/spur-acp/src/connection/native.rs:1622-1651` | Modify | Delete `.toml` after graceful killpg |
| `crates/spur-acp/src/connection/native.rs:1263, 1294, 1605` | Modify | Switch single-PID kill to `killpg` (after registry plumbing) |
| `crates/spur-acp/src/process_inspector.rs` | Create | `ProcessInspector` trait + macOS/Linux impls |
| `crates/spur-acp/src/orphan_sweeper.rs` | Create | `OrphanSweeper::run` |
| `crates/spur-cli/src/main.rs:20-55` | Modify | Call sweeper before agent spawning |
| `crates/spur-core/src/event.rs` (or events module) | Modify | Add `SpurEventBody::OrphanReaped` |
| `crates/spur-tui/src/dashboard.rs:1549` | Modify | Add render arm for `OrphanReaped` |
| `crates/spur-tui/src/dashboard.rs:885-888` | Modify | Extend Ctrl-C handler to also handle Ctrl-Q |
| `crates/spur-tui/src/tui.rs` (or app.rs event loop) | Modify | Subscribe to `tokio::signal::unix` for SIGTERM/SIGHUP/SIGQUIT |
| `.gitignore` | Modify | Add `.spur/pgids/` |
| `crates/spur-acp/tests/orphan_sweep_e2e.rs` | Create | Integration: kill -9 spur → sweep reaps next boot |
| `crates/spur-acp/tests/process_kill_on_drop.rs` | Modify | Add `.toml` lifecycle assertions |

---

## Task 1: Add `process_group(0)` to four missing spawn sites

**Files:**
- Modify: `crates/spur-acp/src/connection/stdio_adapter.rs:107`
- Modify: `crates/spur-acp/src/connection/stream_json_adapter.rs:183`
- Modify: `crates/spur-acp/src/connection/cli_wrap_adapter.rs:188`
- Modify: `crates/spur-acp/src/connection/native.rs:1108`

This is the same-day "stop the bleeding" fix. Independently shippable.

- [ ] **Step 1: Confirm exact line numbers**

Run:
```bash
grep -n "kill_on_drop(true)" crates/spur-acp/src/connection/stdio_adapter.rs
grep -n "kill_on_drop(true)" crates/spur-acp/src/connection/stream_json_adapter.rs
grep -n "kill_on_drop(true)" crates/spur-acp/src/connection/cli_wrap_adapter.rs
grep -n "kill_on_drop(true)" crates/spur-acp/src/connection/native.rs
```

Note the line where each `kill_on_drop(true)` lives. Lines may have drifted from spec citations.

- [ ] **Step 2: Add the same change at all 4 sites**

Immediately after each `.kill_on_drop(true);` add (each separate file):

```rust
#[cfg(unix)]
cmd.process_group(0);
```

In `native.rs`, the `terminal/create` block at ~line 1108 already has `kill_on_drop(true)` but no process group. Add it there too.

- [ ] **Step 3: Run the existing kill-on-drop test**

Run: `cargo test -p spur-acp --test process_kill_on_drop`
Expected: PASS — adding process_group does not change the kill_on_drop semantics for the single-PID case the test exercises.

- [ ] **Step 4: Run the full ACP test suite**

Run: `cargo test -p spur-acp`
Expected: PASS — no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/connection/stdio_adapter.rs \
        crates/spur-acp/src/connection/stream_json_adapter.rs \
        crates/spur-acp/src/connection/cli_wrap_adapter.rs \
        crates/spur-acp/src/connection/native.rs
git commit -m "fix(spur-acp): add process_group(0) to four missing spawn sites

Previously only NativeAcpConnection's main spawn (native.rs:906) put
children in their own pgid. The other three adapters and the ACP
terminal/create block lacked it, so killpg() was a no-op for those
agent types — kill_on_drop reached only the direct PID, leaving node
and the actual agent binary as PPID=1 orphans.

Sites:
- stdio_adapter.rs (StdioAdapter spawn)
- stream_json_adapter.rs (StreamJsonAdapter spawn)
- cli_wrap_adapter.rs (CliWrapAdapter spawn)
- native.rs:1108 (ACP terminal/create)
"
```

---

## Task 2: Add `.spur/pgids/` to `.gitignore`

**Files:**
- Modify: `.gitignore`

- [ ] **Step 1: Add the line**

In the root `.gitignore`, find the existing `.spur/` block (lists `worktrees/`, `events/`, `logs/`, `bot/`, `keys/`). Add:

```
**/.spur/pgids/**
```

- [ ] **Step 2: Verify**

Run: `git check-ignore -v .spur/pgids/anything.toml`
Expected: a match indicating the new pattern.

- [ ] **Step 3: Commit**

```bash
git add .gitignore
git commit -m "chore(gitignore): exclude .spur/pgids/ (orphan-reaping registry)"
```

---

## Task 3: Define `PgidRecord` and registry I/O

**Files:**
- Create: `crates/spur-acp/src/orphan_registry.rs`
- Modify: `crates/spur-acp/src/lib.rs` (declare module)
- Test: inline `#[cfg(test)] mod tests` in the new file

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/src/orphan_registry.rs` with the test stub first:

```rust
//! Durable on-disk registry of pgids for orphan reaping.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_load_round_trips() {
        let dir = tempdir().expect("tmpdir");
        let registry = PgidRegistry::new(dir.path());
        let rec = PgidRecord {
            spur_pid: 81282,
            spur_pid_start_time: 1_745_825_534,
            agent_name: "claude-code".into(),
            cmd: "/opt/homebrew/bin/npm exec @anthropic-ai/claude-agent-acp@0.26.0".into(),
            pgid: 8801,
            pgid_leader_start_time: 1_745_825_534,
            spawned_at: 1_745_825_534,
        };
        registry.write(&rec).expect("write");

        let loaded = registry.load_all().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pgid, 8801);
        assert_eq!(loaded[0].cmd, rec.cmd);
    }

    #[test]
    fn delete_removes_record() {
        let dir = tempdir().expect("tmpdir");
        let registry = PgidRegistry::new(dir.path());
        let rec = PgidRecord {
            spur_pid: 1, spur_pid_start_time: 0,
            agent_name: "a".into(), cmd: "c".into(),
            pgid: 9001, pgid_leader_start_time: 0, spawned_at: 0,
        };
        registry.write(&rec).expect("write");
        registry.delete(rec.pgid).expect("delete");
        assert_eq!(registry.load_all().expect("load").len(), 0);
    }

    #[test]
    fn corrupted_toml_is_skipped_not_panicked() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("9999.toml"), "this is not toml [[[")
            .expect("write garbage");
        let registry = PgidRegistry::new(dir.path());
        // Must not panic. Garbage record yields a warning + skip.
        let loaded = registry.load_all().expect("load");
        assert_eq!(loaded.len(), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib orphan_registry::tests`
Expected: FAIL — module not declared / `PgidRecord` not defined.

- [ ] **Step 3: Implement the module**

Append to `crates/spur-acp/src/orphan_registry.rs` (above the test mod):

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgidRecord {
    pub spur_pid: i32,
    /// Unix epoch seconds — canonical form, `i64`. Mac uses
    /// `proc_bsdinfo.pbi_start_tvsec`; Linux derives from
    /// `/proc/<pid>/stat` field 22 + `/proc/uptime`.
    pub spur_pid_start_time: i64,
    pub agent_name: String,
    /// Full command line (argv joined with spaces).
    pub cmd: String,
    pub pgid: i32,
    pub pgid_leader_start_time: i64,
    pub spawned_at: i64,
}

pub struct PgidRegistry {
    root: PathBuf,
}

impl PgidRegistry {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self { root: root.as_ref().to_path_buf() }
    }

    fn path_for(&self, pgid: i32) -> PathBuf {
        self.root.join(format!("{pgid}.toml"))
    }

    pub fn write(&self, rec: &PgidRecord) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create {}", self.root.display()))?;
        let path = self.path_for(rec.pgid);
        let body = toml::to_string(rec).context("serialize PgidRecord")?;
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn delete(&self, pgid: i32) -> Result<()> {
        let path = self.path_for(pgid);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete {}", path.display())),
        }
    }

    /// Load every parseable `.toml` in the directory. Unparseable files
    /// emit a `warn!` and are skipped (mid-write crash defense).
    pub fn load_all(&self) -> Result<Vec<PgidRecord>> {
        let mut out = Vec::new();
        let read = match std::fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in read {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&path).and_then(|body| {
                toml::from_str::<PgidRecord>(&body)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            }) {
                Ok(rec) => out.push(rec),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "orphan_registry: skipping unparseable record"
                    );
                }
            }
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Declare module in `lib.rs`**

In `crates/spur-acp/src/lib.rs`, add:

```rust
pub mod orphan_registry;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-acp --lib orphan_registry::tests`
Expected: PASS — all three tests green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/orphan_registry.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): durable PgidRecord registry under .spur/pgids/

Stores spawn-time identity evidence (spur_pid + start_time, pgid leader
cmd + start_time) so the next-boot sweep can verify before issuing
killpg. i64 epoch seconds are the canonical form (no locale-dependent
ps shellout). Garbage TOML is skipped, not panicked.
"
```

---

## Task 4: Wire spawn-time write + Drop/shutdown delete

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:907-928` (after `cmd.spawn()`, populate registry)
- Modify: `crates/spur-acp/src/connection/native.rs:358-378` (Drop's killpg → also delete `.toml`)
- Modify: `crates/spur-acp/src/connection/native.rs:1622-1651` (graceful shutdown's killpg → also delete `.toml`)
- Test: extend `crates/spur-acp/tests/process_kill_on_drop.rs`

- [ ] **Step 1: Write the failing test**

In `crates/spur-acp/tests/process_kill_on_drop.rs`, add:

```rust
#[tokio::test]
async fn spawn_writes_pgid_toml_drop_deletes_it() {
    use spur_acp::orphan_registry::PgidRegistry;

    let dir = tempfile::tempdir().expect("tmpdir");
    let pgids = dir.path().join(".spur").join("pgids");

    // Spawn a NativeAcpConnection in this temp root with a cheap mock
    // command that sleeps. (Use the existing test helper if one exists;
    // otherwise stand up a minimal one.)
    let conn = make_test_native_connection(dir.path()).await;

    // After spawn, exactly one record must exist.
    let registry = PgidRegistry::new(&pgids);
    let recs = registry.load_all().expect("load");
    assert_eq!(recs.len(), 1, "expected 1 record after spawn, got {:?}", recs);
    let pgid = recs[0].pgid;

    drop(conn); // triggers killpg + .toml delete

    // Allow drop's spawn_blocking to finalize.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let recs = registry.load_all().expect("load");
    assert_eq!(recs.len(), 0, "expected 0 records after drop, got {:?}", recs);
    let _ = pgid;
}
```

(`make_test_native_connection` is a test-helper to be added in this file. Keep it minimal — spawn `/bin/sh -c 'sleep 30'` as the agent so killpg has a real target.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test process_kill_on_drop spawn_writes_pgid_toml_drop_deletes_it`
Expected: FAIL — `make_test_native_connection` doesn't exist; spawn doesn't write a `.toml`.

- [ ] **Step 3: Add the spawn-time write in `native.rs`**

In `native.rs` after the existing pgid-recording block (`:921-928`), and after fetching `start_time` for the spur process and the pgid leader (use `process_inspector::current_inspector()` once Task 5 lands; for now use the `libproc`/`/proc` reads inline):

```rust
// Persist a registry record for next-boot reconciliation.
let registry = orphan_registry::PgidRegistry::new(
    repo_root.join(".spur").join("pgids"),
);
let rec = orphan_registry::PgidRecord {
    spur_pid: std::process::id() as i32,
    spur_pid_start_time: process_inspector::starttime_of_self(),
    agent_name: agent_name.clone(),
    cmd: format!("{} {}", command, extra_args.join(" ")),
    pgid: pid as i32,
    pgid_leader_start_time: process_inspector::starttime_of(pid as i32)
        .unwrap_or(0),
    spawned_at: chrono::Utc::now().timestamp(),
};
if let Err(e) = registry.write(&rec) {
    tracing::warn!(error = %e, "orphan_registry write failed; sweep cannot reclaim this child");
}
```

(`process_inspector::starttime_of_self()` and `starttime_of()` are introduced in Task 5; until then use a stub that returns `chrono::Utc::now().timestamp()`. Wire properly in Task 5.)

- [ ] **Step 4: Add the delete in Drop and graceful shutdown**

In `Drop for NativeAcpConnection` (currently at `:358-378`), after the `killpg(pgid, "KILL")` call:

```rust
let registry = orphan_registry::PgidRegistry::new(
    self.repo_root.join(".spur").join("pgids"),
);
let _ = registry.delete(pgid);
```

In the graceful `shutdown()` arm (`:1622-1651`), after the second `killpg`:

```rust
let registry = orphan_registry::PgidRegistry::new(
    repo_root.join(".spur").join("pgids"),
);
let _ = registry.delete(pgid);
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-acp --test process_kill_on_drop`
Expected: PASS — both the existing tests and the new `.toml` lifecycle test.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/tests/process_kill_on_drop.rs
git commit -m "feat(spur-acp): wire pgid registry into spawn + Drop + shutdown

- After every NativeAcpConnection spawn, write .spur/pgids/<pgid>.toml.
- After successful killpg in Drop and graceful shutdown, delete the record.
- The on-disk record outlives spur-process death so the next-boot sweep
  can reconcile.

ProcessInspector start_time hooks are stubbed; real impl in next task.
"
```

---

## Task 5: Implement `ProcessInspector` trait + macOS/Linux impls

**Files:**
- Create: `crates/spur-acp/src/process_inspector.rs`
- Modify: `crates/spur-acp/Cargo.toml` (add `libproc` for macOS)
- Modify: `crates/spur-acp/src/lib.rs` (declare module)
- Modify: `crates/spur-acp/src/connection/native.rs` (replace stubs from Task 4)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/src/process_inspector.rs` with tests:

```rust
//! Cross-platform process inspection seam for orphan reaping.

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn live_pid_starttime_is_some() {
        let inspector = production_inspector();
        let me = std::process::id() as i32;
        let st = inspector.starttime_of(me);
        assert!(st.is_some(), "starttime of self should be Some");
    }

    #[test]
    fn dead_pid_starttime_is_none() {
        // Spawn-and-reap to get a definitely-dead PID.
        let mut child = Command::new("/bin/true").spawn().expect("spawn");
        let pid = child.id() as i32;
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let inspector = production_inspector();
        assert_eq!(inspector.starttime_of(pid), None);
    }

    #[test]
    fn cmd_of_self_contains_test_runner() {
        let inspector = production_inspector();
        let cmd = inspector.cmd_of(std::process::id() as i32);
        assert!(cmd.is_some());
        // Self test process; cargo runs as `<deps>/spur_acp-<hash>` typically.
        // Just assert non-empty string.
        assert!(!cmd.unwrap().is_empty());
    }

    #[test]
    fn mock_inspector_threads_through_trait() {
        let mock = MockInspector::with_alive(123, 999, "/bin/test arg");
        assert_eq!(mock.starttime_of(123), Some(999));
        assert_eq!(mock.cmd_of(123), Some("/bin/test arg".to_string()));
        assert_eq!(mock.starttime_of(456), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib process_inspector::tests`
Expected: FAIL — module not declared, types missing.

- [ ] **Step 3: Add `libproc` dep (macOS only)**

In `crates/spur-acp/Cargo.toml`:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
libproc = "0.14"

[target.'cfg(target_os = "linux")'.dependencies]
# /proc reads are pure std::fs; no extra dep needed.
```

- [ ] **Step 4: Implement the trait + production impls**

```rust
use std::collections::HashMap;
use std::sync::Mutex;

/// Cross-platform process inspection seam.
///
/// Production impls read `proc_bsdinfo` on macOS and `/proc/<pid>/stat` +
/// `/proc/uptime` on Linux. Test mock allows deterministic sweep tests.
pub trait ProcessInspector: Send + Sync {
    /// Unix epoch seconds at which the process started. `None` if no live
    /// process holds the PID.
    fn starttime_of(&self, pid: i32) -> Option<i64>;

    /// Full command line (argv joined with spaces). `None` if PID is dead.
    fn cmd_of(&self, pid: i32) -> Option<String>;

    /// Send `signal` to every process in the group whose leader is `pgid`.
    /// ESRCH/EPERM are swallowed (best-effort).
    fn killpg(&self, pgid: i32, signal: Signal);
}

#[derive(Debug, Clone, Copy)]
pub enum Signal { Term, Kill }

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use libproc::proc_pid::{self, BSDInfo, ListPidInfo};

    pub struct MacInspector;
    impl ProcessInspector for MacInspector {
        fn starttime_of(&self, pid: i32) -> Option<i64> {
            let info: BSDInfo = proc_pid::pidinfo(pid, 0).ok()?;
            Some(info.pbi_start_tvsec as i64)
        }
        fn cmd_of(&self, pid: i32) -> Option<String> {
            let path = proc_pid::pidpath(pid).ok()?;
            // libproc::proc_pid::listpidinfo for argv is platform-fiddly;
            // fall back to `path` only and accept that argv-join is approximate.
            // (Followup ticket bd-20k may upgrade to pid_argsv if needed.)
            Some(path)
        }
        fn killpg(&self, pgid: i32, signal: Signal) {
            let sig = match signal { Signal::Term => "TERM", Signal::Kill => "KILL" };
            let _ = std::process::Command::new("kill")
                .arg(format!("-{sig}"))
                .arg(format!("-{pgid}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    pub struct LinuxInspector;
    impl ProcessInspector for LinuxInspector {
        fn starttime_of(&self, pid: i32) -> Option<i64> {
            // /proc/<pid>/stat field 22 = starttime in clock ticks since boot.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // Field 22, but field 2 (comm) can contain spaces in parens.
            let after_paren = stat.rfind(')')?;
            let rest = &stat[after_paren + 2..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            // After comm, fields[0] = state. starttime is field 22 in 1-indexed
            // proc(5); after state it is index 19 in this rest split (state, ppid,
            // pgrp, session, tty_nr, tpgid, flags, minflt, cminflt, majflt, cmajflt,
            // utime, stime, cutime, cstime, priority, nice, num_threads, itrealvalue,
            // starttime).
            let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;
            // Convert to epoch seconds via /proc/uptime + boot time.
            let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
            let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
            let now = chrono::Utc::now().timestamp();
            let boot_epoch = now - uptime_secs as i64;
            // ticks-per-second = sysconf(_SC_CLK_TCK), normally 100.
            let tps = 100i64;
            Some(boot_epoch + (starttime_ticks as i64 / tps))
        }
        fn cmd_of(&self, pid: i32) -> Option<String> {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            // argv elements are NUL-separated.
            let parts: Vec<String> = cmdline
                .split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            if parts.is_empty() { None } else { Some(parts.join(" ")) }
        }
        fn killpg(&self, pgid: i32, signal: Signal) {
            let sig = match signal { Signal::Term => "TERM", Signal::Kill => "KILL" };
            let _ = std::process::Command::new("kill")
                .arg(format!("-{sig}"))
                .arg(format!("-{pgid}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

pub fn production_inspector() -> Box<dyn ProcessInspector> {
    #[cfg(target_os = "macos")]
    { Box::new(mac::MacInspector) }
    #[cfg(target_os = "linux")]
    { Box::new(linux::LinuxInspector) }
}

/// Convenience: starttime of the running spur process.
pub fn starttime_of_self() -> i64 {
    production_inspector()
        .starttime_of(std::process::id() as i32)
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
}

/// Convenience for the spawn site.
pub fn starttime_of(pid: i32) -> Option<i64> {
    production_inspector().starttime_of(pid)
}

/// Hand-rolled mock for unit tests (no `mockall` dep — matches the
/// project's existing pattern).
pub struct MockInspector {
    starttimes: Mutex<HashMap<i32, i64>>,
    cmds: Mutex<HashMap<i32, String>>,
    killed: Mutex<Vec<(i32, Signal)>>,
}

impl MockInspector {
    pub fn with_alive(pid: i32, starttime: i64, cmd: &str) -> Self {
        let mut st = HashMap::new();
        st.insert(pid, starttime);
        let mut cm = HashMap::new();
        cm.insert(pid, cmd.to_string());
        Self {
            starttimes: Mutex::new(st),
            cmds: Mutex::new(cm),
            killed: Mutex::new(Vec::new()),
        }
    }
    pub fn killed(&self) -> Vec<(i32, Signal)> {
        self.killed.lock().unwrap().clone()
    }
}

impl ProcessInspector for MockInspector {
    fn starttime_of(&self, pid: i32) -> Option<i64> {
        self.starttimes.lock().unwrap().get(&pid).copied()
    }
    fn cmd_of(&self, pid: i32) -> Option<String> {
        self.cmds.lock().unwrap().get(&pid).cloned()
    }
    fn killpg(&self, pgid: i32, signal: Signal) {
        self.killed.lock().unwrap().push((pgid, signal));
    }
}

impl Clone for Signal {
    fn clone(&self) -> Self { *self }
}
```

- [ ] **Step 5: Declare module in `lib.rs`**

In `crates/spur-acp/src/lib.rs`:

```rust
pub mod process_inspector;
```

- [ ] **Step 6: Wire into `native.rs` (replace Task 4 stubs)**

Replace the placeholder calls from Task 4 to use the real inspector functions
(`process_inspector::starttime_of_self()`, `process_inspector::starttime_of(pid)`).

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-acp --lib process_inspector`
Run: `cargo test -p spur-acp --test process_kill_on_drop`
Expected: PASS for both.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/Cargo.toml \
        crates/spur-acp/src/process_inspector.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-acp/src/connection/native.rs \
        Cargo.lock
git commit -m "feat(spur-acp): ProcessInspector trait + macOS/Linux impls

- macOS uses libproc::pidinfo for start_time (epoch seconds, not strings).
- Linux reads /proc/<pid>/stat field 22 + /proc/uptime.
- No ps shellout (locale-fragile + O(N) syscall cost on boot).
- Hand-rolled MockInspector for sweep unit tests.
"
```

---

## Task 6: Implement `OrphanSweeper::run` and wire into spur startup

**Files:**
- Create: `crates/spur-acp/src/orphan_sweeper.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-cli/src/main.rs` (call sweeper before agent spawning)
- Test: inline `#[cfg(test)] mod tests` in the sweeper

- [ ] **Step 1: Write the failing tests**

Create `crates/spur-acp/src/orphan_sweeper.rs`:

```rust
//! Orphan tree sweeper: walks .spur/pgids/, kills stale trees safely.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphan_registry::{PgidRegistry, PgidRecord};
    use crate::process_inspector::{MockInspector, Signal};
    use tempfile::tempdir;

    fn make_record(pgid: i32, spur_pid: i32, st: i64, cmd: &str) -> PgidRecord {
        PgidRecord {
            spur_pid,
            spur_pid_start_time: st,
            agent_name: "test".into(),
            cmd: cmd.into(),
            pgid,
            pgid_leader_start_time: st,
            spawned_at: 0,
        }
    }

    #[test]
    fn owner_alive_skip_no_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry.write(&make_record(8001, 1234, 555, "/bin/test")).unwrap();

        // Owner (1234) is alive with matching start_time; pgid leader (8001)
        // also alive with matching start_time + cmd.
        let mut inspector = MockInspector::with_alive(1234, 555, "/proc/spur");
        inspector.add_alive(8001, 555, "/bin/test");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 0);
        assert_eq!(report.skipped_alive_owner, 1);
    }

    #[test]
    fn owner_dead_pgid_match_then_kill_term_then_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry.write(&make_record(8002, 1234, 555, "/bin/test")).unwrap();

        // Owner 1234 is dead (not in inspector). Pgid leader 8002 alive
        // and identity matches.
        let mut inspector = MockInspector::with_alive(8002, 555, "/bin/test");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 1);
        assert_eq!(report.killed[0].pgid, 8002);
        assert!(matches!(
            report.signals_sent[..],
            [(8002, Signal::Term), (8002, Signal::Kill)]
        ));
        // .toml record removed.
        assert_eq!(registry.load_all().unwrap().len(), 0);
    }

    #[test]
    fn pgid_recycled_drops_record_no_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry.write(&make_record(8003, 1234, 555, "/bin/old")).unwrap();

        // Owner dead. Pgid 8003 still alive but has different cmd (recycled).
        let inspector = MockInspector::with_alive(8003, 999, "/bin/different");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 0);
        assert_eq!(report.skipped_recycled, 1);
        // Record should be cleaned up to avoid future false-positives.
        assert_eq!(registry.load_all().unwrap().len(), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-acp --lib orphan_sweeper::tests`
Expected: FAIL — `OrphanSweeper` not defined; `MockInspector::add_alive` does not exist (must be added to Task 5's mock).

- [ ] **Step 3: Add `MockInspector::add_alive` (Task 5 helper)**

In `crates/spur-acp/src/process_inspector.rs`, add:

```rust
impl MockInspector {
    pub fn add_alive(&mut self, pid: i32, starttime: i64, cmd: &str) {
        self.starttimes.lock().unwrap().insert(pid, starttime);
        self.cmds.lock().unwrap().insert(pid, cmd.to_string());
    }
}
```

- [ ] **Step 4: Implement `OrphanSweeper`**

Append to `crates/spur-acp/src/orphan_sweeper.rs`:

```rust
use crate::orphan_registry::{PgidRecord, PgidRegistry};
use crate::process_inspector::{ProcessInspector, Signal};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct OrphanSweeper {
    registry: PgidRegistry,
    inspector: Box<dyn ProcessInspector>,
    grace_period: Duration,
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub killed: Vec<PgidRecord>,
    pub skipped_alive_owner: usize,
    pub skipped_recycled: usize,
    pub unparseable: usize,
    pub signals_sent: Vec<(i32, Signal)>,
}

impl OrphanSweeper {
    pub fn new(pgids_dir: impl AsRef<Path>, inspector: Box<dyn ProcessInspector>) -> Self {
        Self {
            registry: PgidRegistry::new(pgids_dir.as_ref()),
            inspector,
            grace_period: Duration::from_millis(250),
        }
    }

    pub fn run(&self) -> SweepReport {
        let mut report = SweepReport::default();
        let records = match self.registry.load_all() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "orphan_sweeper: registry load failed");
                return report;
            }
        };

        for rec in records {
            // 1. Is owning spur alive?
            match self.inspector.starttime_of(rec.spur_pid) {
                Some(st) if st == rec.spur_pid_start_time => {
                    report.skipped_alive_owner += 1;
                    continue;
                }
                _ => {} // dead OR recycled → fall through
            }

            // 2. Is the recorded pgid leader still the same process?
            let leader_now = self.inspector.starttime_of(rec.pgid);
            let leader_cmd = self.inspector.cmd_of(rec.pgid);
            if leader_now != Some(rec.pgid_leader_start_time)
                || leader_cmd.as_deref() != Some(rec.cmd.as_str())
            {
                report.skipped_recycled += 1;
                let _ = self.registry.delete(rec.pgid);
                continue;
            }

            // 3. Reap.
            self.inspector.killpg(rec.pgid, Signal::Term);
            report.signals_sent.push((rec.pgid, Signal::Term));
            std::thread::sleep(self.grace_period);
            self.inspector.killpg(rec.pgid, Signal::Kill);
            report.signals_sent.push((rec.pgid, Signal::Kill));
            let _ = self.registry.delete(rec.pgid);
            tracing::warn!(
                agent = %rec.agent_name,
                pgid = rec.pgid,
                age_secs = chrono::Utc::now().timestamp() - rec.spawned_at,
                "orphan_sweeper: reaped stale agent tree"
            );
            report.killed.push(rec);
        }

        report
    }
}
```

Make `Signal` derive `PartialEq` for the assert in tests. Add to Task 5's `Signal` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal { Term, Kill }
```

- [ ] **Step 5: Declare module in `lib.rs`**

In `crates/spur-acp/src/lib.rs`:

```rust
pub mod orphan_sweeper;
```

- [ ] **Step 6: Wire into spur startup**

In `crates/spur-cli/src/main.rs`, after `init_tracing()` returns and before any agent spawning:

```rust
{
    use spur_acp::orphan_sweeper::OrphanSweeper;
    use spur_acp::process_inspector::production_inspector;

    let pgids_dir = repo_root.join(".spur").join("pgids");
    let report = OrphanSweeper::new(&pgids_dir, production_inspector()).run();
    if !report.killed.is_empty() {
        tracing::warn!(
            killed = report.killed.len(),
            recycled = report.skipped_recycled,
            "orphan_sweeper: cleaned up stale agent trees from prior session"
        );
        // Emit one SpurEvent::OrphanReaped per killed tree (added in Task 7).
    }
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-acp --lib orphan_sweeper`
Run: `cargo test --workspace --lib`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/src/orphan_sweeper.rs \
        crates/spur-acp/src/process_inspector.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-cli/src/main.rs
git commit -m "feat(spur-acp): OrphanSweeper with identity-verified killpg

- Walks .spur/pgids/ at every spur startup.
- Three-step gate: owner alive? → pgid leader identity match? → reap.
- Uses ProcessInspector for testable, mockable, cross-platform semantics.
- 250ms grace between SIGTERM and SIGKILL.
- Wired into init_tracing()-adjacent boot sequence in spur-cli.

Closes the orphan-accumulation defect: every spur boot now reconciles
trees the previous session leaked via SIGKILL/OOM.
"
```

---

## Task 7: Add `SpurEventBody::OrphanReaped` + dashboard render arm

**Files:**
- Modify: `crates/spur-core/src/events.rs` (or wherever `SpurEventBody` is defined)
- Modify: `crates/spur-tui/src/dashboard.rs:1549` (add render arm)
- Modify: `crates/spur-cli/src/main.rs` (emit event from sweep report)

- [ ] **Step 1: Locate `SpurEventBody`**

Run: `grep -rn "enum SpurEventBody" crates/`
Note the file + line.

- [ ] **Step 2: Add the variant**

In the `SpurEventBody` enum, add:

```rust
OrphanReaped {
    agent_name: String,
    pgid: i32,
    age_secs: i64,
},
```

Confirm the enum is `#[non_exhaustive]` (existing). If not, downstream matches will fail to compile.

- [ ] **Step 3: Add render arm in `dashboard.rs:1549`**

In the existing `match SpurEventBody { ... }` block, add:

```rust
SpurEventBody::OrphanReaped { agent_name, pgid, age_secs } => {
    push_log_entry(&mut activity_log, format!(
        "Reaped orphan {agent_name} (pgid {pgid}, age {age_secs}s)"
    ));
}
```

(`push_log_entry` matches the helper used by neighboring arms; align with existing convention.)

- [ ] **Step 4: Emit from sweep**

In `crates/spur-cli/src/main.rs` (the sweep-call block from Task 6), emit one event per killed tree:

```rust
for rec in &report.killed {
    let _ = event_sender.send(SpurEvent {
        seq: 0, // assigned by the sink
        body: SpurEventBody::OrphanReaped {
            agent_name: rec.agent_name.clone(),
            pgid: rec.pgid,
            age_secs: chrono::Utc::now().timestamp() - rec.spawned_at,
        },
        timestamp: chrono::Utc::now(),
    });
}
```

(The `event_sender` is the same channel `Orchestrator` uses today; locate via `grep -n "event_sender" crates/spur-cli/src/main.rs` and follow the existing pattern.)

- [ ] **Step 5: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS — non_exhaustive ensures no compile breakage; dashboard arm renders correctly.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/events.rs \
        crates/spur-tui/src/dashboard.rs \
        crates/spur-cli/src/main.rs
git commit -m "feat(spur-core,spur-tui): OrphanReaped SpurEvent + TUI render arm

Dashboard activity log now surfaces orphan-sweep results so users see
that the cleanup happened. Without an explicit match arm, the new
variant would compile under #[non_exhaustive]'s catch-all and be
invisible.
"
```

---

## Task 8: Crossterm-event-driven shutdown for SIGTERM/SIGHUP/SIGQUIT

**Files:**
- Modify: `crates/spur-tui/src/tui.rs` (or app.rs event loop)
- Modify: `crates/spur-tui/src/dashboard.rs:885-888` (extend Ctrl-C arm)

- [ ] **Step 1: Confirm existing Ctrl-C handling**

Read `crates/spur-tui/src/dashboard.rs:885-888`. Confirm it matches `KeyModifiers::CONTROL` + `KeyCode::Char('c')`. Note the surrounding event-loop owner.

- [ ] **Step 2: Add Ctrl-Q to the same arm**

Extend the match arm to also handle `KeyCode::Char('q')` with the same shutdown action.

- [ ] **Step 3: Wire signal handlers into the event loop**

In `crates/spur-tui/src/tui.rs` (or wherever the event loop lives), where the crossterm `EventStream` is awaited, also `select!` on three `tokio::signal::unix::Signal` futures:

```rust
use tokio::signal::unix::{signal, SignalKind};

let mut sigterm = signal(SignalKind::terminate()).expect("sigterm");
let mut sighup = signal(SignalKind::hangup()).expect("sighup");
let mut sigquit = signal(SignalKind::quit()).expect("sigquit");

let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

// Bridge each signal into the bounded shutdown channel.
{
    let tx = shutdown_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sigterm.recv() => { let _ = tx.try_send(()); }
                _ = sighup.recv() => { let _ = tx.try_send(()); }
                _ = sigquit.recv() => { let _ = tx.try_send(()); }
            }
        }
    });
}

// In the main loop:
loop {
    tokio::select! {
        Some(event) = events.next() => {
            // existing crossterm event handling
        }
        _ = shutdown_rx.recv() => {
            // Same path as Ctrl-C: tear down crossterm raw mode + alt screen,
            // then exit cleanly.
            tear_down_terminal();
            std::process::exit(0);
        }
    }
}
```

`mpsc::channel(1)` coalesces duplicate signals via `try_send` — `Err(Full(_))` is fine because the first signal already triggered shutdown.

- [ ] **Step 4: Verify behaviorally**

Run: `cargo build -p spur-cli --release`
Then in two terminals:
- `./target/release/spur tui` in terminal 1.
- `kill -HUP <pid>` from terminal 2.

Expected: spur exits cleanly without leaving the terminal in raw mode.

Also test `kill -TERM <pid>` and `kill -QUIT <pid>` separately. SIGKILL remains uncatchable; the on-startup sweep is the safety net.

- [ ] **Step 5: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS — adding signal handling does not break existing tests.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/tui.rs crates/spur-tui/src/dashboard.rs
git commit -m "feat(spur-tui): graceful shutdown on Ctrl-Q + SIGTERM/SIGHUP/SIGQUIT

- Extend Ctrl-C key arm to also handle Ctrl-Q (q in raw mode).
- tokio::signal::unix handlers for SIGTERM/SIGHUP/SIGQUIT push into a
  bounded mpsc(1) bridged into the event loop.
- All paths run the same teardown (raw mode → alt screen → drop
  Orchestrator → exit), so terminal state is always restored.
- SIGHUP coverage closes the iTerm-tab-close orphan source.
- No signal_hook dependency added (verified zero source refs today).
"
```

---

## Task 9: Integration test — kill -9 spur → next boot reaps

**Files:**
- Create: `crates/spur-acp/tests/orphan_sweep_e2e.rs`

This is the load-bearing acceptance test for the spec's primary
acceptance criterion: "After kill -9 of a running spur tui, the next
spur tui startup reaps every orphan tree from the prior session within
1 second."

- [ ] **Step 1: Write the integration test**

Create `crates/spur-acp/tests/orphan_sweep_e2e.rs`:

```rust
//! End-to-end: spawn spur as a child, induce orphan via kill -9, verify
//! the next boot's sweep reaps it.

use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;

#[test]
#[ignore] // Requires the spur binary; run explicitly with `cargo test --test orphan_sweep_e2e -- --ignored`.
fn kill_9_spur_then_reboot_reaps_orphan() {
    let dir = tempdir().expect("tmpdir");

    // Step 1: spawn spur tui; let it start a sleeping mock agent.
    let mut spur = Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["tui", "--brain", "mock-sleep"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn spur");

    // Wait for the .spur/pgids/<pgid>.toml to appear.
    let pgids_dir = dir.path().join(".spur").join("pgids");
    for _ in 0..50 {
        if pgids_dir.exists() && std::fs::read_dir(&pgids_dir)
            .map(|r| r.count() > 0).unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let recs_before: Vec<_> = std::fs::read_dir(&pgids_dir)
        .expect("pgids dir")
        .collect::<Result<_, _>>().expect("read");
    assert!(!recs_before.is_empty(), "spur did not register a pgid record");

    // Step 2: Capture the pgid of the agent for the orphan check.
    let pgid_path = recs_before[0].path();
    let body = std::fs::read_to_string(&pgid_path).expect("read");
    let pgid_line = body.lines().find(|l| l.starts_with("pgid")).expect("pgid");
    let pgid: i32 = pgid_line.split('=').nth(1).unwrap().trim().parse().unwrap();

    // Step 3: SIGKILL spur. Drop never runs.
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(spur.id() as i32),
        nix::sys::signal::Signal::SIGKILL,
    ).expect("sigkill");
    let _ = spur.wait();

    // Step 4: Verify the orphan is alive (PPID=1 reparented).
    std::thread::sleep(Duration::from_millis(200));
    let alive = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pgid),
        None,
    ).is_ok();
    assert!(alive, "expected pgid {pgid} alive after spur SIGKILL");

    // Step 5: Re-spawn spur; the startup sweep must reap.
    let mut spur2 = Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["tui", "--brain", "mock-sleep", "--exit-after-sweep"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn spur 2nd time");

    // Wait up to 2 seconds for the sweep to run + agent to die.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut reaped = false;
    while std::time::Instant::now() < deadline {
        if nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pgid),
            None,
        ).is_err() {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = spur2.kill();
    let _ = spur2.wait();

    assert!(reaped, "orphan pgid {pgid} not reaped within 2s of spur restart");
}
```

(The `--exit-after-sweep` flag is a test-only escape that runs the
sweep and exits before entering the TUI loop. Add it to the CLI's
`Cli::parse()` as a hidden flag in this task.)

- [ ] **Step 2: Add the `--exit-after-sweep` hidden CLI flag**

In `crates/spur-cli/src/main.rs`, add to the TUI subcommand:

```rust
#[arg(long, hide = true)]
exit_after_sweep: bool,
```

After the sweep block (Task 6), if `exit_after_sweep`:

```rust
if exit_after_sweep {
    return Ok(());
}
```

- [ ] **Step 3: Add `nix` to dev-dependencies**

In `crates/spur-acp/Cargo.toml`:

```toml
[dev-dependencies]
nix = { version = "0.29", features = ["signal", "process"] }
```

(Or matching whatever version is already on `Cargo.lock`.)

- [ ] **Step 4: Run the test (manually, with --ignored)**

Run: `cargo test -p spur-acp --test orphan_sweep_e2e -- --ignored`
Expected: PASS — orphan reaped within 2s.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/tests/orphan_sweep_e2e.rs \
        crates/spur-acp/Cargo.toml \
        crates/spur-cli/src/main.rs \
        Cargo.lock
git commit -m "test(spur-acp): integration test for kill-9 → next-boot sweep

#[ignore]'d by default (requires spawning real spur binary). Run with
'cargo test --test orphan_sweep_e2e -- --ignored' in CI nightlies.

Adds hidden --exit-after-sweep flag to spur tui so the test does not
have to rendezvous with the full TUI lifecycle.
"
```

---

## Task 10: Manual verification + acceptance check

**Files:**
- None modified; verification ritual.

- [ ] **Step 1: Reproduce the original incident, verify fix**

```bash
# 1. Clean slate
rm -rf .spur/pgids

# 2. Start spur tui, let it spawn one agent
cargo run --release -p spur-cli -- tui --brain claude-code &
SPUR_PID=$!
sleep 5

# 3. SIGKILL spur (the way the original incident happened)
kill -9 $SPUR_PID

# 4. Confirm orphans exist
ps -axo pid,ppid,etime,command | awk '$2==1 && /codex-acp|claude-agent-acp/'
# Expect: ≥ 1 process listed

# 5. Restart spur — the sweep should reap them
cargo run --release -p spur-cli -- tui --brain claude-code &
SPUR_PID2=$!
sleep 3
kill $SPUR_PID2

# 6. Confirm orphans gone
ps -axo pid,ppid,etime,command | awk '$2==1 && /codex-acp|claude-agent-acp/'
# Expect: empty
```

- [ ] **Step 2: Tab-close test**

Open spur tui in iTerm/Terminal. Close the tab (sends SIGHUP). Confirm:
- TUI exits without trapping the next prompt in raw mode.
- `ps` shows no orphan agent processes.

- [ ] **Step 3: Tick acceptance criteria in the spec**

Open `docs/superpowers/specs/2026-04-28-orphan-reaping-design.md` and tick each acceptance-criteria checkbox. Commit:

```bash
git add docs/superpowers/specs/2026-04-28-orphan-reaping-design.md
git commit -m "docs(spec): tick orphan-reaping acceptance criteria after Tasks 1-9"
```

---

## Self-review checklist (run before declaring done)

- [ ] All steps numbered and bite-sized.
- [ ] No "TBD", "implement later", "similar to Task N" placeholders.
- [ ] Every code step shows actual code.
- [ ] File paths are exact.
- [ ] Test commands have expected output.
- [ ] Each task ends with a commit step.
- [ ] Spec coverage: every component change in `2026-04-28-orphan-reaping-design.md`
      maps to a task — process_group(0)×4 = T1, gitignore = T2, registry I/O = T3,
      spawn-time write = T4, ProcessInspector = T5, sweeper + startup wiring = T6,
      SpurEventBody + dashboard = T7, crossterm-event shutdown = T8, kill-9 e2e = T9,
      manual verification = T10.
- [ ] Sequencing honored: Task 1 (process_group only) is independently shippable
      same-day. Tasks 2-9 follow the registry → inspector → sweeper → events →
      shutdown order; do NOT switch the kill sites at native.rs:1263/1294/1605
      to killpg until Task 4's pgid is plumbed through (deferred to bd-20k as
      it requires cross-plumbing the recorded pgid into those sites).

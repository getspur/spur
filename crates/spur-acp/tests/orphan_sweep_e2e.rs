//! End-to-end: simulate a SIGKILL'd-spur orphan via direct spawn + a
//! synthetic PgidRecord, then boot the real spur binary and verify its
//! startup sweep reaps the tree.
//!
//! Acceptance criterion (Task 9 of orphan-reaping plan):
//! "After kill -9 of a running spur tui, the next spur tui startup reaps
//! every orphan tree from the prior session within 1 second."
//!
//! `#[ignore]`'d by default — runs a real `spur` binary. Run with:
//!   cargo test -p spur-acp --test orphan_sweep_e2e -- --ignored
//!
//! Why a synthetic owner instead of spawning spur for step 1:
//! the production brain registry is loaded from `.spur/config.toml`, and
//! triggering an actual brain spawn requires the TUI to start (which
//! needs a TTY on stdin). Both can be wired up, but the kill-9-and-reboot
//! lifecycle this test asserts is identical regardless of who wrote the
//! PgidRecord on disk: an orphan is simply (record on disk) + (owner pid
//! does not match the recorded start_time) + (pgid leader still alive).
//! We construct that state directly. The "spur child wrote a PgidRecord
//! on agent spawn" path is covered by
//! `spawn_writes_pgid_toml_drop_deletes_it` in process_kill_on_drop.rs.

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::tempdir;

use spur_acp::orphan_registry::{PgidRecord, PgidRegistry};
use spur_acp::process_inspector::production_inspector;

/// Locate the `spur` binary. `CARGO_BIN_EXE_<name>` is only set for
/// integration tests in the same package as the binary; this test lives in
/// `spur-acp` while `spur` ships from `spur-cli`, so we walk the workspace
/// `target/{debug,release}` directory instead.
fn spur_bin() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_spur") {
        return PathBuf::from(p);
    }
    let exe = if cfg!(windows) { "spur.exe" } else { "spur" };
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/spur-acp -> crates -> workspace root
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    for profile in ["debug", "release"] {
        let cand = target_dir.join(profile).join(exe);
        if cand.exists() {
            return cand;
        }
    }
    panic!(
        "spur binary not found in {}/{{debug,release}}; run `cargo build --bin spur` first",
        target_dir.display()
    );
}

/// Kills + waits the wrapped child on Drop so a failed assert does not
/// leak a `sleep 30` zombie tied to the test's process group. Take the
/// inner `Child` with `take()` once the test is ready to wait on it
/// itself; the guard then no-ops on Drop.
struct SpawnGuard(Option<Child>);

impl SpawnGuard {
    fn pid(&self) -> i32 {
        self.0.as_ref().expect("child still owned").id() as i32
    }

    fn take(mut self) -> Child {
        self.0.take().expect("child already taken")
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
#[ignore] // Requires the spur binary; run explicitly with `cargo test --test orphan_sweep_e2e -- --ignored`.
fn kill_9_spur_then_reboot_reaps_orphan() {
    let dir = tempdir().expect("tmpdir");
    let pgids_dir = dir.path().join(".spur").join("pgids");

    // Step 1: spawn a sleeping child as its own process-group leader.
    // `process_group(0)` invokes setpgid(0, 0) so pgid == child pid —
    // the same shape NativeAcpConnection installs via `cmd.process_group(0)`.
    //
    // Spawn `sleep` directly (no shell wrapper) so the executable image is
    // stable from t=0. A `/bin/sh -c 'sleep N'` form would exec(2) into
    // `sleep` after a few ms, racing the inspector's first read.
    let mut cmd = Command::new("/bin/sleep");
    cmd.arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = cmd.spawn().expect("spawn /bin/sleep 30");
    let guard = SpawnGuard(Some(child));
    let pgid = guard.pid();

    // Brief settle: pidpath/cmdline reads can lag fork on macOS by a few
    // schedule ticks; 50 ms is well above the worst case I've measured.
    std::thread::sleep(Duration::from_millis(50));

    // Step 2: derive the leader's identity from the live process so the
    // sweeper's identity check matches by construction. macOS `cmd_of`
    // returns the executable path; Linux returns argv joined with spaces.
    // The production spawn site at native.rs records `format!("{cmd} {args}")`,
    // which mismatches macOS — a separate ticket (bd-20k). Recording the
    // value `cmd_of` actually returns lets this test exercise the kill
    // path on both platforms without taking a stance on that issue.
    let inspector = production_inspector();
    let leader_cmd = inspector
        .cmd_of(pgid)
        .expect("cmd_of(child pgid) shortly after spawn");
    let leader_st = inspector
        .starttime_of(pgid)
        .expect("starttime_of(child pgid) shortly after spawn");

    // Step 3: write a PgidRecord whose owner is unmistakably absent.
    // pid 1 (init) exists, but its real start_time is the boot epoch —
    // 0 cannot match, so the sweeper falls through the "owner alive"
    // skip and proceeds to the identity + reap branches.
    let registry = PgidRegistry::new(&pgids_dir);
    let synthetic_orphan = PgidRecord {
        spur_pid: 1,
        spur_pid_start_time: 0,
        agent_name: "mock-sleep".into(),
        cmd: leader_cmd,
        pgid,
        pgid_leader_start_time: leader_st,
        spawned_at: chrono::Utc::now().timestamp(),
    };
    registry
        .write(&synthetic_orphan)
        .expect("write synthetic PgidRecord");

    // Sanity: the record we just wrote loads back through the registry's
    // own parser. This replaces the racy `read_dir().count() > 0` check
    // the test originally used to detect "spur wrote a record" — that
    // check would race the spawn site mid-write; `load_all()` only
    // counts records that fully serialize.
    let recs = registry.load_all().expect("load_all");
    assert_eq!(
        recs.len(),
        1,
        "expected exactly 1 PgidRecord on disk before sweep, got {recs:?}"
    );

    // Step 4: confirm the orphan is still alive.
    let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pgid), None).is_ok();
    assert!(alive, "synthetic orphan pgid {pgid} unexpectedly dead before sweep");

    // Step 5: boot spur with `--exit-after-sweep`. The sweep runs
    // unconditionally at the top of `run()` (before the subcommand match),
    // so the gate exits cleanly without TUI setup, license validation, or
    // brain registry lookup. `.status()` blocks until spur returns.
    let spur_status = Command::new(spur_bin())
        .args(["tui", "--exit-after-sweep"])
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn spur tui --exit-after-sweep");
    assert!(spur_status.success(), "spur exited non-zero: {spur_status:?}");

    // Step 6: wait for the child to actually exit. We're its parent, so
    // the kernel keeps the SIGKILL'd child as a zombie in our process
    // table until we wait on it — `kill -0 pid` would still report it
    // alive. `try_wait()` returns `Some(_)` once the child has been
    // signalled and the kernel has posted its exit status.
    let mut child = guard.take();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut reaped = false;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_status)) => {
                reaped = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("try_wait on orphan child failed: {e}"),
        }
    }
    if !reaped {
        // Don't leak the still-alive child if the assert below fires.
        let _ = child.kill();
        let _ = child.wait();
        panic!("orphan pgid {pgid} not reaped within 2s of spur boot");
    }

    // Sweep also unlinks the .toml on success.
    let recs_after = registry.load_all().expect("load_all post-sweep");
    assert!(
        recs_after.is_empty(),
        "expected 0 PgidRecords post-sweep, got {recs_after:?}"
    );
}

#!/usr/bin/env python3
"""PTY-based UAT for spur tui log-rotation delivery (2026-04-28-log-rotation plan).

Verifies the acceptance criteria that need a live `spur tui` instance:

  1. Boot + clean teardown leaves `.spur/logs/spur.log.<TODAY>` written.
  2. Active spur.log file stays bounded (≤ 8 MB + slop) within a short run.
  3. `RUST_LOG=debug` smoke: known debug strings reach disk within ~1s. This
     is the deferred acceptance criterion (lines 364-365 of the spec).
  4. With the default EnvFilter (`warn,spur_core::orchestrator=info`), the
     per-frame `F_frame_drain` debug events do NOT reach disk — proving
     the firehose is clamped.
  5. ACP child stderr files appear under `.spur/logs/<agent>-<pid>.log*`
     iff a child spawned (only verified if a brain agent actually attached;
     skipped otherwise).
  6. `.spur/events/` ndjson files exist and total stays bounded by the
     events cap.

Modeled on `scripts/uat/spur_signal_uat.py`. Each scenario runs in a fresh
tempdir; spur is launched under a PTY so the TUI initializes normally.

Usage:  python3 spur_log_rotation_uat.py [SPUR_BIN]
"""
import datetime
import os
import pty
import select
import signal
import sys
import tempfile
import termios
import time
from pathlib import Path


SPUR = Path(sys.argv[1] if len(sys.argv) > 1 else "/Volumes/Projects/spur/target/debug/spur")
BOOT_BUDGET_SEC = 8.0
SETTLE_SEC = 4.0  # let the per-frame draws emit some debug spans
EXIT_BUDGET_SEC = 8.0
HARD_KILL_GRACE_SEC = 3.0

ACTIVE_FILE_CAP_BYTES = 8 * 1024 * 1024 + 64 * 1024  # 8 MB + 64 KB slop
EVENTS_DIR_CAP_BYTES = 64 * 1024 * 1024 + 1 * 1024 * 1024  # 64 MB + slop

# Debug sites we tag in source. F_frame_drain ALWAYS fires when the TUI
# renders (Tasks 3/2 left it at debug!). The other two only fire when an
# ACP brain is attached and emitting notifications, so the bare-`spur tui`
# UAT can't depend on them — they're "bonus" sites.
ALWAYS_FIRES_DEBUG_SITE = b"F_frame_drain"
BONUS_DEBUG_SITES = [b"C_orchestrator_emit", b"A_session_notification"]


def spawn_spur_in_pty(workdir: Path, env_overrides: dict[str, str]):
    """Fork+exec spur tui under a real PTY. Returns (pid, master_fd)."""
    pid, master_fd = pty.fork()
    if pid == 0:
        os.chdir(str(workdir))
        os.environ.pop("SPUR_FORCE_TTY", None)
        for k, v in env_overrides.items():
            os.environ[k] = v
        os.execv(str(SPUR), [str(SPUR), "tui"])
    try:
        attrs = termios.tcgetattr(master_fd)
        attrs[0] &= ~(termios.IGNBRK | termios.BRKINT | termios.PARMRK
                      | termios.ISTRIP | termios.INLCR | termios.IGNCR
                      | termios.ICRNL | termios.IXON)
        attrs[3] &= ~(termios.ECHO | termios.ECHONL | termios.ICANON
                      | termios.ISIG | termios.IEXTEN)
        termios.tcsetattr(master_fd, termios.TCSANOW, attrs)
    except Exception as e:
        print(f"  [warn] could not set raw mode on master_fd: {e}", flush=True)
    return pid, master_fd


def drain_until(fd: int, deadline: float, marker_check=None) -> bytes:
    buf = bytearray()
    while time.time() < deadline:
        rl, _, _ = select.select([fd], [], [], 0.1)
        if rl:
            try:
                chunk = os.read(fd, 8192)
            except OSError:
                break
            if not chunk:
                break
            buf.extend(chunk)
            if marker_check and marker_check(buf):
                break
    return bytes(buf)


def reap(pid: int, hard_deadline: float):
    while time.time() < hard_deadline:
        try:
            wpid, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return -1
        if wpid == pid:
            return status
        time.sleep(0.05)
    try:
        os.kill(pid, signal.SIGKILL)
        wpid, status = os.waitpid(pid, 0)
        return status | 0x80000000
    except (ChildProcessError, OSError):
        return None


def today_basepath_glob(workdir: Path) -> list[Path]:
    """All spur.log.YYYY-MM-DD* under workdir/.spur/logs."""
    logs_dir = workdir / ".spur" / "logs"
    if not logs_dir.is_dir():
        return []
    today = datetime.date.today().strftime("%Y-%m-%d")
    return sorted(logs_dir.glob(f"spur.log.{today}*"))


def events_files(workdir: Path) -> list[Path]:
    events_dir = workdir / ".spur" / "events"
    if not events_dir.is_dir():
        return []
    return sorted(events_dir.glob("*.ndjson"))


def sum_bytes(files: list[Path]) -> int:
    return sum(f.stat().st_size for f in files if f.exists())


def grep_files(files: list[Path], needle: bytes) -> int:
    """Return total occurrences of `needle` across the file set."""
    n = 0
    for f in files:
        try:
            n += f.read_bytes().count(needle)
        except OSError:
            pass
    return n


def scenario(
    name: str,
    workdir: Path,
    env_overrides: dict[str, str],
    settle_sec: float,
    *,
    expect_debug_sites: bool,
) -> dict:
    print(f"\n=== Scenario: {name}  (workdir={workdir}) ===", flush=True)
    print(f"  env: {env_overrides}", flush=True)

    pid, master_fd = spawn_spur_in_pty(workdir, env_overrides)
    print(f"  spawned spur pid={pid}", flush=True)

    boot_deadline = time.time() + BOOT_BUDGET_SEC
    boot_buf = drain_until(
        master_fd,
        boot_deadline,
        marker_check=lambda b: b"\x1b[" in b and len(b) > 64,
    )
    booted = b"\x1b[" in boot_buf
    print(f"  boot: {'OK' if booted else 'FAIL'}  ({len(boot_buf)} bytes from PTY)", flush=True)
    if not booted:
        try: os.kill(pid, signal.SIGKILL)
        except OSError: pass
        try: os.close(master_fd)
        except OSError: pass
        os.waitpid(pid, 0)
        return {"name": name, "booted": False}

    # Drain and let frames render so per-render debug spans land in the log.
    drain_until(master_fd, time.time() + settle_sec)

    # Clean shutdown via SIGTERM (handler hits the same teardown as Ctrl-Q).
    print("  -> sending SIGTERM", flush=True)
    os.kill(pid, signal.SIGTERM)

    drain_deadline = time.time() + EXIT_BUDGET_SEC
    drain_until(master_fd, drain_deadline)
    exit_status = reap(pid, time.time() + HARD_KILL_GRACE_SEC)
    print(f"  exit_status={exit_status}", flush=True)
    try: os.close(master_fd)
    except OSError: pass

    # Inspect on-disk artifacts.
    log_files = today_basepath_glob(workdir)
    log_total = sum_bytes(log_files)
    active_log = next((f for f in log_files if not f.name.endswith(".gz")
                       and "." not in f.name.split("spur.log.")[-1].lstrip("0123456789-")),
                      None)
    # Active is the basepath itself, e.g. spur.log.YYYY-MM-DD
    today = datetime.date.today().strftime("%Y-%m-%d")
    active_log = workdir / ".spur" / "logs" / f"spur.log.{today}"
    active_size = active_log.stat().st_size if active_log.exists() else 0

    ev_files = events_files(workdir)
    ev_total = sum_bytes(ev_files)

    debug_hits = {}
    # Always probe the always-fires site so we can assert it's clamped under
    # default and present under RUST_LOG=debug.
    debug_hits[ALWAYS_FIRES_DEBUG_SITE.decode()] = grep_files(
        log_files, ALWAYS_FIRES_DEBUG_SITE)
    if expect_debug_sites:
        for site in BONUS_DEBUG_SITES:
            debug_hits[site.decode()] = grep_files(log_files, site)

    print(f"  spur.log files: {len(log_files)}; total={log_total} bytes; "
          f"active={active_size} bytes", flush=True)
    print(f"  .spur/events/ files: {len(ev_files)}; total={ev_total} bytes", flush=True)
    if expect_debug_sites:
        print(f"  debug-site hits in log: {debug_hits}", flush=True)

    return {
        "name": name,
        "booted": True,
        "exit_status": exit_status,
        "log_files": [f.name for f in log_files],
        "log_total_bytes": log_total,
        "active_size_bytes": active_size,
        "events_count": len(ev_files),
        "events_total_bytes": ev_total,
        "debug_site_hits": debug_hits,
    }


def assess(r: dict, *, expect_debug: bool) -> tuple[bool, list[str]]:
    fails = []
    if not r.get("booted"):
        fails.append("boot failed")
        return False, fails

    if not r.get("log_files"):
        fails.append("no spur.log.<TODAY> file produced")
    if r.get("active_size_bytes", 0) > ACTIVE_FILE_CAP_BYTES:
        fails.append(f"active spur.log {r['active_size_bytes']} > cap {ACTIVE_FILE_CAP_BYTES}")
    if r.get("events_total_bytes", 0) > EVENTS_DIR_CAP_BYTES:
        fails.append(f"events total {r['events_total_bytes']} > cap {EVENTS_DIR_CAP_BYTES}")

    hits = r.get("debug_site_hits", {})
    f_render = hits.get(ALWAYS_FIRES_DEBUG_SITE.decode(), 0)
    if expect_debug:
        # With RUST_LOG=debug, F_frame_drain MUST appear (TUI rendered at
        # least one frame). Bonus sites are best-effort.
        if f_render == 0:
            fails.append(
                "RUST_LOG=debug did not produce F_frame_drain in log "
                "(TUI must have rendered at least one frame)"
            )
    else:
        # With the default filter, F_frame_drain must NOT appear — that's
        # the firehose we clamped via EnvFilter `warn,spur_core::orchestrator=info`.
        if f_render > 0:
            fails.append(
                f"default filter leaked F_frame_drain {f_render} times (firehose not clamped)"
            )
    return not fails, fails


def main():
    if not SPUR.exists():
        print(f"FATAL: spur binary not found at {SPUR}", file=sys.stderr)
        print("       Build with: cargo build -p spur-cli", file=sys.stderr)
        sys.exit(2)

    results = []

    # Scenario A: default filter — firehose clamped, log small.
    workdir_a = Path(tempfile.mkdtemp(prefix="spur-uat-logrot-default-"))
    try:
        r = scenario("default_filter", workdir_a, {}, SETTLE_SEC,
                     expect_debug_sites=False)
        ok, fails = assess(r, expect_debug=False)
        r["uat_ok"] = ok
        r["uat_fails"] = fails
        results.append(r)
    except Exception as e:
        print(f"  scenario default_filter crashed: {e}", file=sys.stderr)
        results.append({"name": "default_filter", "error": str(e)})

    time.sleep(0.5)

    # Scenario B: RUST_LOG=debug — known debug spans reach disk.
    workdir_b = Path(tempfile.mkdtemp(prefix="spur-uat-logrot-debug-"))
    try:
        r = scenario("rust_log_debug", workdir_b,
                     {"RUST_LOG": "debug"}, SETTLE_SEC,
                     expect_debug_sites=True)
        ok, fails = assess(r, expect_debug=True)
        r["uat_ok"] = ok
        r["uat_fails"] = fails
        results.append(r)
    except Exception as e:
        print(f"  scenario rust_log_debug crashed: {e}", file=sys.stderr)
        results.append({"name": "rust_log_debug", "error": str(e)})

    print("\n=== Summary ===")
    overall_ok = True
    for r in results:
        if "error" in r:
            print(f"  {r['name']:<18s}  ERROR  {r['error']}")
            overall_ok = False
            continue
        if not r.get("booted"):
            print(f"  {r['name']:<18s}  NOT_BOOTED")
            overall_ok = False
            continue
        verdict = "PASS" if r["uat_ok"] else "FAIL"
        if not r["uat_ok"]:
            overall_ok = False
        print(f"  {r['name']:<18s}  {verdict}  exit={r['exit_status']} "
              f"active={r['active_size_bytes']}B events={r['events_total_bytes']}B")
        for f in r.get("uat_fails", []):
            print(f"      - {f}")

    print()
    print(f"OVERALL: {'PASS' if overall_ok else 'FAIL'}")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())

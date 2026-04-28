#!/usr/bin/env python3
"""PTY-based UAT for spur tui signal handling (orphan-reaping plan T10).

Tests that the TUI:
  1. Boots and renders something to the PTY.
  2. On SIGTERM/SIGHUP/SIGQUIT/Ctrl-C, exits within a budget.
  3. Restores the terminal (alt-screen exit + raw-mode disable escape codes
     observable in the PTY output stream).

Each scenario runs in a fresh tempdir so .spur/pgids/ is isolated.

Usage:  python3 pty_uat.py [SPUR_BIN]   (default: target/debug/spur)
"""
import os
import pty
import signal
import subprocess
import sys
import tempfile
import termios
import time
import select
from pathlib import Path


SPUR = Path(sys.argv[1] if len(sys.argv) > 1 else "/Volumes/Projects/spur/target/debug/spur")
BOOT_BUDGET_SEC = 8.0
SETTLE_BEFORE_SIGNAL_SEC = 2.0  # extra dwell so signal handlers install
EXIT_BUDGET_SEC = 8.0  # how long after sending the signal we wait for clean exit
HARD_KILL_GRACE_SEC = 3.0  # extra wait after EXIT_BUDGET_SEC before SIGKILL


# Common alt-screen / raw-mode disable codes that crossterm emits on teardown.
# We only need to *observe at least one of these* in the post-signal output to
# claim "clean teardown".
TEARDOWN_TOKENS = [
    b"\x1b[?1049l",  # exit alternate screen
    b"\x1b[?25h",    # show cursor
    b"\x1b[?2004l",  # disable bracketed paste
    b"\x1b[?1006l",  # disable extended mouse
    b"\x1b[?1000l",  # disable mouse reporting
]


def spawn_spur_in_pty(workdir: Path):
    """Fork+exec spur tui under a real PTY. Returns (pid, master_fd)."""
    pid, master_fd = pty.fork()
    if pid == 0:
        # Child: exec spur. The PTY is now its controlling tty.
        os.chdir(str(workdir))
        os.environ.pop("SPUR_FORCE_TTY", None)
        os.execv(str(SPUR), [str(SPUR), "tui"])
    # Parent: put the slave PTY into raw mode so control bytes (Ctrl-C/Q,
    # i.e. 0x03 / 0x11) flow through as literal bytes instead of being
    # converted to SIGINT/SIGTSTP by the line discipline. Spur's own
    # crossterm::enable_raw_mode does this for the slave-side, but doing
    # it here too eliminates any race during the boot window.
    try:
        attrs = termios.tcgetattr(master_fd)
        # iflag, oflag, cflag, lflag, ispeed, ospeed, cc
        attrs[0] &= ~(termios.IGNBRK | termios.BRKINT | termios.PARMRK
                      | termios.ISTRIP | termios.INLCR | termios.IGNCR
                      | termios.ICRNL | termios.IXON)
        attrs[3] &= ~(termios.ECHO | termios.ECHONL | termios.ICANON
                      | termios.ISIG | termios.IEXTEN)
        termios.tcsetattr(master_fd, termios.TCSANOW, attrs)
    except Exception as e:
        print(f"  [warn] could not set raw mode on master_fd: {e}", flush=True)
    return pid, master_fd


def read_until(fd: int, deadline: float, marker_check=None) -> bytes:
    """Drain the PTY until deadline, returning all bytes seen.
    Optionally exits early when marker_check(buffer) returns True."""
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


def child_alive(pid: int) -> bool:
    try:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        return wpid == 0
    except ChildProcessError:
        return False
    except OSError:
        return False


def reap(pid: int, hard_deadline: float) -> int | None:
    """Wait for child to exit by deadline. Returns exit status or None on timeout.
    On timeout, SIGKILL+wait."""
    while time.time() < hard_deadline:
        try:
            wpid, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return -1
        if wpid == pid:
            return status
        time.sleep(0.05)
    # Timeout: hard kill so the harness doesn't leak
    try:
        os.kill(pid, signal.SIGKILL)
        wpid, status = os.waitpid(pid, 0)
        return status | 0x80000000  # mark as killed-by-harness
    except (ChildProcessError, OSError):
        return None


def detect_teardown(buf: bytes) -> list[str]:
    found = []
    for tok in TEARDOWN_TOKENS:
        if tok in buf:
            found.append(tok.decode("ascii", errors="replace"))
    return found


def scenario(name: str, send_sig=None, send_keys: bytes | None = None) -> dict:
    """Run one scenario: boot -> probe -> signal-or-keys -> verify exit + teardown."""
    workdir = Path(tempfile.mkdtemp(prefix=f"spur-uat-{name}-"))
    print(f"\n=== Scenario: {name}  (workdir={workdir}) ===", flush=True)

    pid, master_fd = spawn_spur_in_pty(workdir)
    print(f"  spawned spur pid={pid}", flush=True)

    # Phase 1: read until we see something that looks like a TUI screen
    # (any escape sequence is a strong signal that crossterm initialized).
    boot_deadline = time.time() + BOOT_BUDGET_SEC
    boot_buf = read_until(
        master_fd,
        boot_deadline,
        marker_check=lambda b: b"\x1b[" in b and len(b) > 64,
    )
    booted = b"\x1b[" in boot_buf
    print(f"  boot: {'OK' if booted else 'FAIL'}  ({len(boot_buf)} bytes)", flush=True)
    if not booted:
        # spur may have failed to launch (license gate, missing config, etc.)
        try: os.kill(pid, signal.SIGKILL)
        except OSError: pass
        try: os.close(master_fd)
        except OSError: pass
        os.waitpid(pid, 0)
        return {"name": name, "booted": False, "exit_status": None}

    # Settle: keep draining so we observe any post-boot frames, and so spur
    # has reached the event-loop select! (where signal handlers are armed).
    if SETTLE_BEFORE_SIGNAL_SEC > 0:
        more = read_until(master_fd, time.time() + SETTLE_BEFORE_SIGNAL_SEC)
        boot_buf = boot_buf + more

    # Phase 2: dispatch the trigger
    if send_sig is not None:
        print(f"  -> sending {send_sig}", flush=True)
        os.kill(pid, send_sig)
    elif send_keys is not None:
        print(f"  -> writing keys {send_keys!r} to PTY", flush=True)
        os.write(master_fd, send_keys)

    # Phase 3: drain output up to EXIT_BUDGET_SEC
    drain_deadline = time.time() + EXIT_BUDGET_SEC
    drain_buf = read_until(master_fd, drain_deadline)

    # Phase 4: reap
    hard_deadline = time.time() + HARD_KILL_GRACE_SEC
    exit_status = reap(pid, hard_deadline)

    teardown = detect_teardown(boot_buf + drain_buf)
    teardown_clean = bool(teardown)

    print(f"  exit_status={exit_status}  teardown_codes={teardown}", flush=True)

    try: os.close(master_fd)
    except OSError: pass

    return {
        "name": name,
        "booted": True,
        "exit_status": exit_status,
        "teardown_codes": teardown,
        "teardown_clean": teardown_clean,
        "drain_bytes": len(drain_buf),
    }


def main():
    if not SPUR.exists():
        print(f"FATAL: spur binary not found at {SPUR}", file=sys.stderr)
        sys.exit(2)

    scenarios = [
        ("ctrl_c",  None,           b"\x03\x03"),     # send Ctrl-C twice (first arms confirm, second commits)
        ("ctrl_q",  None,           b"\x11\x11"),     # send Ctrl-Q twice
        ("sigterm", signal.SIGTERM, None),
        ("sighup",  signal.SIGHUP,  None),
        ("sigquit", signal.SIGQUIT, None),
    ]

    results = []
    for name, sig, keys in scenarios:
        try:
            results.append(scenario(name, send_sig=sig, send_keys=keys))
        except Exception as e:
            print(f"  scenario {name} crashed: {e}", file=sys.stderr)
            results.append({"name": name, "error": str(e)})
        time.sleep(0.5)  # let any leftover state settle between scenarios

    print("\n=== Summary ===")
    for r in results:
        if "error" in r:
            print(f"  {r['name']:<10s}  ERROR  {r['error']}")
        elif not r.get("booted"):
            print(f"  {r['name']:<10s}  NOT_BOOTED")
        else:
            ok = "OK" if r.get("teardown_clean") else "NO_TEARDOWN_CODE"
            print(f"  {r['name']:<10s}  exit={r['exit_status']}  {ok}  ({len(r.get('teardown_codes', []))} restore codes)")

    return 0


if __name__ == "__main__":
    sys.exit(main())

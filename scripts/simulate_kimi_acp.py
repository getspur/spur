#!/usr/bin/env python3
"""
Probe `kimi [-y] acp` to map its ACP-over-stdio behavior to spur-acp handlers.

Spawns kimi in ACP mode (optionally with `-y` for "yolo" / auto-approve),
performs the JSON-RPC handshake, sends a tool-forcing prompt, auto-replies
to any `session/request_permission` requests, and tabulates every
`session/update` notification variant the server emits.

The final report cross-references each observed variant against the
spur-acp dispatch table to flag UNHANDLED variants that would silently
drop in the TUI today.

Sources of truth for the mapping (do not drift):
  - crates/spur-acp/src/connection/native.rs::session_update_variant_name (line 1903)
  - crates/spur-tui/src/components/react_trace/dispatch.rs::dispatch_session_update (line 45)

Usage:
  python3 scripts/simulate_kimi_acp.py [--yolo] [--prompt TEXT]
                                       [--timeout SECONDS] [--out PATH]
                                       [--quiet]

Examples:
  # Baseline (no yolo): expect permission requests on every tool use.
  python3 scripts/simulate_kimi_acp.py

  # Yolo mode: empirically test whether `-y` suppresses session/request_permission.
  python3 scripts/simulate_kimi_acp.py --yolo

  # Custom prompt + capture raw JSONL for diff:
  python3 scripts/simulate_kimi_acp.py --yolo \
      --prompt "List the files in the current directory using your tools." \
      --out .spur/logs/probe-kimi-yolo.jsonl
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
import uuid
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional


def _utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def _utc_now_compact() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")

# ── spur-acp dispatch mapping (mirror of dispatch.rs + native.rs) ──────────
# Keep this aligned with:
#   crates/spur-tui/src/components/react_trace/dispatch.rs::dispatch_session_update
#   crates/spur-acp/src/connection/native.rs::session_update_variant_name
SPUR_DISPATCH = {
    # session/update variant : (handler description, "HANDLED"|"NOOP"|"CALLER")
    "agent_thought_chunk":      ("trace.append_think()",                     "HANDLED"),
    "agent_message_chunk":      ("trace.append_message()",                   "HANDLED"),
    "user_message_chunk":       ("trace.append_user_message()",              "HANDLED"),
    "tool_call":                ("trace.push(TraceKind::Act)",               "HANDLED"),
    "tool_call_update":         ("trace.find_act_by_id_mut()/synthesize Act","HANDLED"),
    "plan":                     ("trace.push(TraceKind::Think summary)",     "HANDLED"),
    "available_commands_update":("session-scoped — handled in caller",       "CALLER"),
    "config_option_update":     ("no-op in dispatch",                        "NOOP"),
    "current_mode_update":      ("session-scoped — handled in caller",       "CALLER"),
    "session_info_update":      ("no-op in dispatch",                        "NOOP"),
    "usage_update":             ("session-scoped — handled in caller",       "CALLER"),
}

# JSON-RPC server-initiated request methods spur-acp expects to handle.
# Anything else is a surprise and worth flagging.
KNOWN_SERVER_REQUESTS = {
    "session/request_permission",
    "fs/read_text_file",
    "fs/write_text_file",
    "terminal/create",
    "terminal/output",
    "terminal/release",
    "terminal/wait_for_exit",
    "terminal/kill",
}

# Default prompt designed to FORCE a tool call so we can observe
# tool_call / tool_call_update / session/request_permission behavior.
DEFAULT_PROMPT = (
    "Use your file-reading tool to read the file Cargo.toml at the workspace "
    "root, then tell me how many workspace members it declares. Do not guess — "
    "use the tool."
)


# ── JSON-RPC plumbing ──────────────────────────────────────────────────────

def make_request(id_: int, method: str, params: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def make_response(id_: int, result: Optional[dict] = None,
                  error: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    return msg


class AcpClient:
    """Thin JSON-RPC client over a kimi acp stdio subprocess."""

    def __init__(self, proc: subprocess.Popen, raw_log_path: Optional[Path],
                 quiet: bool):
        # The Popen call site requests stdin/stdout/stderr pipes, so these
        # are guaranteed non-None — assert to satisfy the type checker.
        assert proc.stdin is not None and proc.stdout is not None and proc.stderr is not None
        self.proc = proc
        self.stdin = proc.stdin
        self.stdout = proc.stdout
        self.stderr = proc.stderr
        self.responses: dict[int, dict] = {}
        self.notifications: list[dict] = []
        self.server_requests: list[dict] = []  # JSON-RPC requests FROM server TO us
        self.raw_log = open(raw_log_path, "w") if raw_log_path else None
        self.quiet = quiet
        self._lock = threading.Lock()
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._stderr = threading.Thread(target=self._stderr_loop, daemon=True)
        self._reader.start()
        self._stderr.start()

    # -- I/O ---------------------------------------------------------------

    def _log(self, direction: str, obj: dict) -> None:
        if self.raw_log is not None:
            self.raw_log.write(json.dumps({
                "ts": _utc_now_iso(),
                "dir": direction,
                "msg": obj,
            }) + "\n")
            self.raw_log.flush()

    def _read_loop(self) -> None:
        # kimi emits one JSON object per line over stdout.
        for raw in self.stdout:
            line = raw.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                if not self.quiet:
                    print(f"[reader] non-JSON: {line[:200]}", file=sys.stderr)
                continue
            self._log("recv", obj)
            with self._lock:
                if "method" in obj and "id" in obj:
                    # Server-to-client request — needs a response.
                    self.server_requests.append(obj)
                elif "method" in obj:
                    self.notifications.append(obj)
                elif "id" in obj:
                    self.responses[obj["id"]] = obj

    def _stderr_loop(self) -> None:
        for raw in self.stderr:
            line = raw.rstrip()
            if line and not self.quiet:
                print(f"[stderr] {line}", file=sys.stderr)

    def send(self, obj: dict) -> None:
        line = json.dumps(obj, separators=(",", ":"))
        self.stdin.write(line + "\n")
        self.stdin.flush()
        self._log("send", obj)
        if not self.quiet:
            print(f"[send] {line[:200]}")

    def wait_response(self, id_: int, timeout: float) -> Optional[dict]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self._lock:
                if id_ in self.responses:
                    return self.responses.pop(id_)
            time.sleep(0.02)
        return None

    def close(self) -> None:
        try:
            self.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        if self.raw_log is not None:
            self.raw_log.close()


# ── Probe driver ───────────────────────────────────────────────────────────

def run_probe(yolo: bool, prompt: str, timeout: float,
              raw_log_path: Optional[Path], quiet: bool) -> int:
    cmd = ["kimi"]
    if yolo:
        cmd.append("-y")
    cmd.append("acp")

    print("=" * 72)
    print(f"KIMI ACP PROBE  (cmd: {' '.join(cmd)})")
    print(f"yolo={yolo}  prompt={prompt!r}")
    print(f"raw_log={raw_log_path}")
    print("=" * 72)

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        cwd=os.getcwd(),
    )
    client = AcpClient(proc, raw_log_path, quiet)

    permission_requests = 0
    other_server_requests: list[str] = []
    notif_counts: dict[str, int] = defaultdict(int)
    notif_text_len: dict[str, int] = defaultdict(int)
    notif_samples: dict[str, list[str]] = defaultdict(list)
    unknown_notif_methods: dict[str, int] = defaultdict(int)

    try:
        # 1. initialize ----------------------------------------------------
        client.send(make_request(0, "initialize", {
            "protocolVersion": 1,
            "clientInfo": {"name": "spur-probe", "version": "0.1.0"},
            "clientCapabilities": {
                # Mirror what spur-acp advertises so kimi behaves the same way it
                # does for real spur sessions.
                "fs": {"readTextFile": True, "writeTextFile": True},
                "terminal": True,
            },
        }))
        init_resp = client.wait_response(0, 15)
        if init_resp is None:
            print("[FAIL] initialize timed out", file=sys.stderr)
            return 1
        print(f"[recv] initialize result: "
              f"{json.dumps(init_resp.get('result', {}))[:300]}")

        # 2. session/new ---------------------------------------------------
        session_id_req = str(uuid.uuid4())
        client.send(make_request(1, "session/new", {
            "sessionId": session_id_req,
            "cwd": os.getcwd(),
            "mcpServers": [],
        }))
        sess_resp = client.wait_response(1, 30)
        if sess_resp is None:
            print("[FAIL] session/new timed out", file=sys.stderr)
            return 1
        print(f"[recv] session/new result: "
              f"{json.dumps(sess_resp.get('result', {}))[:300]}")
        session_id = sess_resp.get("result", {}).get("sessionId", session_id_req)

        # 3. drain pre-prompt notifications (available_commands etc.) -----
        time.sleep(0.5)
        with client._lock:
            preamble = client.notifications[:]
            client.notifications.clear()
        for n in preamble:
            method = n.get("method", "")
            if method == "session/update":
                variant = n.get("params", {}).get("update", {}).get(
                    "sessionUpdate", "unknown")
                print(f"[preamble] session/update: {variant}")
                notif_counts[variant] += 1
            else:
                print(f"[preamble] {method}")
                unknown_notif_methods[method] += 1

        # 4. session/prompt ------------------------------------------------
        client.send(make_request(2, "session/prompt", {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": prompt}],
        }))

        # 5. event loop until prompt response arrives or timeout ----------
        start = time.time()
        prompt_resp: Optional[dict] = None
        while time.time() - start < timeout:
            # Drain server-to-client requests (permission / fs / terminal).
            with client._lock:
                inbound_requests = client.server_requests[:]
                client.server_requests.clear()
            for req in inbound_requests:
                method = req.get("method", "")
                rid = req.get("id")
                if not isinstance(rid, int):
                    if not quiet:
                        print(f"[warn] server request without integer id: "
                              f"{json.dumps(req)[:200]}", file=sys.stderr)
                    continue
                if method == "session/request_permission":
                    permission_requests += 1
                    options = (req.get("params", {})
                                .get("options", []) or [])
                    # Prefer allow_always > allow_once > first option, matching
                    # against either `kind` (ACP-spec field) or `optionId`
                    # (kimi uses values like "approve_for_session"/"approve").
                    chosen = None
                    for pref in ("allow_always", "allow_once", "allow"):
                        for opt in options:
                            if opt.get("kind") == pref or opt.get("optionId") == pref:
                                chosen = opt.get("optionId") or opt.get("id")
                                break
                        if chosen:
                            break
                    if not chosen and options:
                        chosen = options[0].get("optionId") or options[0].get("id")
                    tool_title = (req.get("params", {})
                                  .get("toolCall", {})
                                  .get("title", "?"))
                    if not quiet:
                        print(f"[req#{rid}] session/request_permission "
                              f"(#{permission_requests}) tool={tool_title!r} "
                              f"→ optionId={chosen!r}")
                    client.send(make_response(rid, {
                        "outcome": {"outcome": "selected",
                                    "optionId": chosen or "approve_for_session"}
                    }))
                else:
                    other_server_requests.append(method)
                    if not quiet:
                        print(f"[req#{rid}] {method} (unhandled by probe — "
                              f"replying with error)")
                    client.send(make_response(rid, error={
                        "code": -32601,
                        "message": f"probe does not implement {method}",
                    }))

            # Drain notifications.
            with client._lock:
                inbound_notifs = client.notifications[:]
                client.notifications.clear()
            for n in inbound_notifs:
                method = n.get("method", "")
                if method != "session/update":
                    unknown_notif_methods[method] += 1
                    if not quiet:
                        print(f"[notif] non-session-update: {method}")
                    continue
                update = n.get("params", {}).get("update", {})
                variant = update.get("sessionUpdate", "unknown")
                notif_counts[variant] += 1
                # Best-effort text extraction for samples.
                content = update.get("content")
                text = ""
                if isinstance(content, dict):
                    text = content.get("text") or ""
                elif isinstance(content, list) and content:
                    if isinstance(content[0], dict):
                        text = content[0].get("text") or ""
                notif_text_len[variant] += len(text)
                if len(notif_samples[variant]) < 3 and text:
                    notif_samples[variant].append(text[:120])
                if not quiet:
                    print(f"[notif] {variant:30s} text_len={len(text):4d} "
                          f"text={text[:80]!r}")

            # Check prompt response.
            with client._lock:
                if 2 in client.responses:
                    prompt_resp = client.responses.pop(2)
                    break
            time.sleep(0.05)

        if prompt_resp is None:
            print(f"[WARN] session/prompt did not complete within {timeout}s",
                  file=sys.stderr)
        else:
            print(f"[recv] session/prompt result: "
                  f"{json.dumps(prompt_resp.get('result', {}))[:300]}")

    finally:
        client.close()

    # ── Report ────────────────────────────────────────────────────────────
    print("\n" + "=" * 72)
    print(f"REPORT  (yolo={yolo})")
    print("=" * 72)
    print(f"session/request_permission requests received: {permission_requests}")
    if yolo and permission_requests > 0:
        print("  ⚠️  `-y` did NOT suppress permission prompts — yolo flag is "
              "ignored or unsupported in `kimi acp` mode.")
    elif yolo and permission_requests == 0:
        print("  ✅ `-y` suppressed all permission prompts in `kimi acp` mode.")
    elif not yolo and permission_requests > 0:
        print("  ✅ baseline behaves as expected (server asks permission).")
    elif not yolo and permission_requests == 0:
        print("  ⚠️  baseline received no permission prompts — either kimi "
              "auto-approves by default or the prompt didn't trigger any "
              "permissioned tools.")

    if other_server_requests:
        print(f"\nOther server-initiated requests: "
              f"{len(other_server_requests)}")
        for method in sorted(set(other_server_requests)):
            count = other_server_requests.count(method)
            known = "known" if method in KNOWN_SERVER_REQUESTS else "UNKNOWN"
            print(f"  - {method:35s} ×{count:3d}   [{known}]")

    if unknown_notif_methods:
        print("\nNon-`session/update` notification methods:")
        for method, n in sorted(unknown_notif_methods.items()):
            print(f"  - {method:35s} ×{n}")

    print("\n`session/update` variants observed → spur-acp dispatch:")
    print("-" * 72)
    if not notif_counts:
        print("  (none)")
    else:
        # Header.
        print(f"  {'variant':30s} {'count':>6s}  "
              f"{'status':<8s} handler")
        for variant, count in sorted(notif_counts.items(),
                                      key=lambda kv: -kv[1]):
            handler, status = SPUR_DISPATCH.get(
                variant, ("(no mapping in spur-acp)", "UNHANDLED"))
            tag = {
                "HANDLED":   "✅",
                "CALLER":    "🟡",
                "NOOP":      "·",
                "UNHANDLED": "❌",
            }.get(status, "?")
            print(f"  {variant:30s} {count:>6d}  "
                  f"{tag} {status:<6s} {handler}")
            for sample in notif_samples.get(variant, []):
                print(f"    └ sample: {sample!r}")
    print("-" * 72)
    print("Legend:  ✅ HANDLED in dispatch_session_update   "
          "🟡 CALLER (session-scoped)   "
          "· NOOP (deliberate)   ❌ UNHANDLED (silent drop)")
    print()
    return 0


# ── CLI ────────────────────────────────────────────────────────────────────

def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--yolo", "-y", action="store_true",
                        help="Spawn `kimi -y acp` (yolo / auto-approve).")
    parser.add_argument("--prompt", default=DEFAULT_PROMPT,
                        help="Prompt to send (default forces a tool call).")
    parser.add_argument("--timeout", type=float, default=180.0,
                        help="Seconds to wait for the prompt to complete.")
    parser.add_argument("--out", type=Path, default=None,
                        help="Path to write raw JSONL of every JSON-RPC frame.")
    parser.add_argument("--quiet", action="store_true",
                        help="Suppress per-message logging; only show report.")
    args = parser.parse_args()

    raw_log_path = args.out
    if raw_log_path is None:
        Path(".spur/logs").mkdir(parents=True, exist_ok=True)
        suffix = "yolo" if args.yolo else "baseline"
        raw_log_path = Path(
            f".spur/logs/probe-kimi-{suffix}-{_utc_now_compact()}.jsonl")

    return run_probe(
        yolo=args.yolo,
        prompt=args.prompt,
        timeout=args.timeout,
        raw_log_path=raw_log_path,
        quiet=args.quiet,
    )


if __name__ == "__main__":
    sys.exit(main())

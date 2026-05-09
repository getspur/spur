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
  - crates/spur-acp/src/connection/native.rs::session_update_variant_name
  - crates/spur-tui/src/components/react_trace/dispatch.rs::dispatch_session_update

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
import signal
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

JsonRpcId = str | int


def new_request_id() -> str:
    """Match the Rust ACP SDK, which uses UUID string JSON-RPC ids."""
    return str(uuid.uuid4())


def make_request(id_: JsonRpcId, method: str, params: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def make_response(id_: JsonRpcId, result: Optional[dict] = None,
                  error: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    return msg


def slice_text(text: str, line: Optional[int], limit: Optional[int]) -> str:
    """Mirror NativeAcpConnection's fs/read_text_file line slicing."""
    lines = text.splitlines()
    start = max((line or 1) - 1, 0)
    selected = lines[start:]
    if limit is not None:
        selected = selected[:max(limit, 0)]
    return "\n".join(selected)


class ProbeRequestError(Exception):
    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def _optional_int(value: Any) -> Optional[int]:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


class TerminalState:
    def __init__(self, proc: subprocess.Popen, output_byte_limit: Optional[int]):
        assert proc.stdout is not None and proc.stderr is not None
        self.proc = proc
        self.output_byte_limit = output_byte_limit
        self.output = ""
        self.truncated = False
        self._lock = threading.Lock()
        self._stdout = threading.Thread(
            target=self._read_stream, args=(proc.stdout,), daemon=True)
        self._stderr = threading.Thread(
            target=self._read_stream, args=(proc.stderr,), daemon=True)
        self._stdout.start()
        self._stderr.start()

    def _read_stream(self, stream: Any) -> None:
        while True:
            chunk = stream.read(4096)
            if not chunk:
                return
            self.append(chunk)

    def append(self, text: str) -> None:
        with self._lock:
            self.output += text
            if self.output_byte_limit is not None and len(self.output) > self.output_byte_limit:
                self.output = self.output[-self.output_byte_limit:]
                self.truncated = True

    def exit_status(self) -> Optional[dict]:
        code = self.proc.poll()
        if code is None:
            return None
        if code < 0:
            try:
                sig = signal.Signals(-code).name
            except ValueError:
                sig = str(-code)
            return {"signal": sig}
        return {"exitCode": code}

    def output_response(self) -> dict:
        with self._lock:
            result = {"output": self.output, "truncated": self.truncated}
        status = self.exit_status()
        if status is not None:
            result["exitStatus"] = status
        return result

    def wait_response(self) -> dict:
        self.proc.wait()
        return self.exit_status() or {}

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()


class ProbeContext:
    """Client-side handlers for requests that `kimi acp` sends to SPUR."""

    def __init__(self, cwd: Path, allow_writes: bool = False):
        self.cwd = cwd
        self.allow_writes = allow_writes
        self.terminals: dict[str, TerminalState] = {}

    def handle_client_request(self, req: dict) -> dict:
        method = req.get("method", "")
        params = req.get("params", {}) or {}
        if method == "fs/read_text_file":
            return self.read_text_file(params)
        if method == "fs/write_text_file":
            return self.write_text_file(params)
        if method == "terminal/create":
            return self.terminal_create(params)
        if method == "terminal/output":
            return self.terminal_output(params)
        if method == "terminal/wait_for_exit":
            return self.terminal_wait_for_exit(params)
        if method == "terminal/kill":
            return self.terminal_kill(params)
        if method == "terminal/release":
            return self.terminal_release(params)
        raise ProbeRequestError(-32601, f"probe does not implement {method}")

    def resolve_path(self, raw_path: str) -> Path:
        path = Path(raw_path).expanduser()
        if path.is_absolute():
            return path
        return self.cwd.joinpath(path)

    def read_text_file(self, params: dict) -> dict:
        raw_path = params.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            raise ProbeRequestError(-32602, "fs/read_text_file requires path")
        path = self.resolve_path(raw_path)
        try:
            content = path.read_text()
        except Exception as exc:
            raise ProbeRequestError(
                -32603, f"Failed to read {path}: {exc}") from exc
        return {
            "content": slice_text(
                content,
                _optional_int(params.get("line")),
                _optional_int(params.get("limit")),
            )
        }

    def write_text_file(self, params: dict) -> dict:
        if not self.allow_writes:
            raise ProbeRequestError(
                -32603,
                "fs/write_text_file denied by probe (pass --allow-writes to enable)",
            )
        raw_path = params.get("path")
        content = params.get("content")
        if not isinstance(raw_path, str) or not isinstance(content, str):
            raise ProbeRequestError(
                -32602, "fs/write_text_file requires string path and content")
        path = self.resolve_path(raw_path)
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        except Exception as exc:
            raise ProbeRequestError(
                -32603, f"Failed to write {path}: {exc}") from exc
        return {}

    def terminal_create(self, params: dict) -> dict:
        command = params.get("command")
        if not isinstance(command, str) or not command:
            raise ProbeRequestError(-32602, "terminal/create requires command")
        args = params.get("args") or []
        if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
            raise ProbeRequestError(-32602, "terminal/create args must be strings")
        cwd = self.resolve_path(params["cwd"]) if isinstance(params.get("cwd"), str) else self.cwd
        env = os.environ.copy()
        for item in params.get("env") or []:
            if isinstance(item, dict) and isinstance(item.get("name"), str):
                env[item["name"]] = str(item.get("value", ""))
        limit = _optional_int(
            params.get("outputByteLimit", params.get("output_byte_limit")))
        if limit is None:
            limit = 10 * 1024 * 1024
        try:
            proc = subprocess.Popen(
                [command, *args],
                cwd=str(cwd),
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                errors="replace",
                bufsize=1,
            )
        except Exception as exc:
            raise ProbeRequestError(
                -32603, f"Failed to spawn {command!r}: {exc}") from exc
        terminal_id = str(uuid.uuid4())
        self.terminals[terminal_id] = TerminalState(proc, limit)
        return {"terminalId": terminal_id}

    def get_terminal(self, params: dict) -> TerminalState:
        terminal_id = params.get("terminalId", params.get("terminal_id"))
        if not isinstance(terminal_id, str) or terminal_id not in self.terminals:
            raise ProbeRequestError(-32602, f"Terminal {terminal_id!r} not found")
        return self.terminals[terminal_id]

    def terminal_output(self, params: dict) -> dict:
        return self.get_terminal(params).output_response()

    def terminal_wait_for_exit(self, params: dict) -> dict:
        return self.get_terminal(params).wait_response()

    def terminal_kill(self, params: dict) -> dict:
        self.get_terminal(params).kill()
        return {}

    def terminal_release(self, params: dict) -> dict:
        terminal_id = params.get("terminalId", params.get("terminal_id"))
        term = self.get_terminal(params)
        term.kill()
        self.terminals.pop(terminal_id, None)
        return {}

    def close(self) -> None:
        for term in self.terminals.values():
            term.kill()


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
        self.responses: dict[JsonRpcId, dict] = {}
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

    def wait_response(self, id_: JsonRpcId, timeout: float) -> Optional[dict]:
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

def run_probe(yolo: bool, afk: bool, prompt: str, timeout: float,
              raw_log_path: Optional[Path], quiet: bool,
              allow_writes: bool = False) -> int:
    cmd = ["kimi"]
    if yolo:
        cmd.append("-y")
    if afk:
        cmd.append("--afk")
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
    ctx = ProbeContext(Path(os.getcwd()), allow_writes=allow_writes)

    permission_requests = 0
    other_server_requests: list[str] = []
    notif_counts: dict[str, int] = defaultdict(int)
    notif_text_len: dict[str, int] = defaultdict(int)
    notif_samples: dict[str, list[str]] = defaultdict(list)
    unknown_notif_methods: dict[str, int] = defaultdict(int)

    try:
        # 1. initialize ----------------------------------------------------
        init_id = new_request_id()
        client.send(make_request(init_id, "initialize", {
            "protocolVersion": 1,
            "clientInfo": {"name": "spur-probe", "version": "0.1.0"},
            "clientCapabilities": {
                # Mirror what spur-acp advertises so kimi behaves the same way it
                # does for real spur sessions.
                "fs": {"readTextFile": True, "writeTextFile": True},
                "terminal": True,
            },
        }))
        init_resp = client.wait_response(init_id, 15)
        if init_resp is None:
            print("[FAIL] initialize timed out", file=sys.stderr)
            return 1
        print(f"[recv] initialize result: "
              f"{json.dumps(init_resp.get('result', {}))[:300]}")

        # 2. session/new ---------------------------------------------------
        session_id_req = str(uuid.uuid4())
        new_session_id = new_request_id()
        client.send(make_request(new_session_id, "session/new", {
            "sessionId": session_id_req,
            "cwd": os.getcwd(),
            "mcpServers": [],
        }))
        sess_resp = client.wait_response(new_session_id, 30)
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
        prompt_id = new_request_id()
        client.send(make_request(prompt_id, "session/prompt", {
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
                if rid is None:
                    if not quiet:
                        print(f"[warn] server request without id: "
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
                    try:
                        result = ctx.handle_client_request(req)
                    except ProbeRequestError as exc:
                        if not quiet:
                            print(f"[req#{rid}] {method} -> error {exc.message}")
                        client.send(make_response(rid, error={
                            "code": exc.code,
                            "message": exc.message,
                        }))
                    else:
                        if not quiet:
                            print(f"[req#{rid}] {method} -> ok")
                        client.send(make_response(rid, result))

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
                if prompt_id in client.responses:
                    prompt_resp = client.responses.pop(prompt_id)
                    break
            time.sleep(0.05)

        if prompt_resp is None:
            print(f"[WARN] session/prompt did not complete within {timeout}s",
                  file=sys.stderr)
        else:
            print(f"[recv] session/prompt result: "
                  f"{json.dumps(prompt_resp.get('result', {}))[:300]}")

    finally:
        ctx.close()
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
                        help="Spawn `kimi -y acp` (yolo / auto-approve tool calls).")
    parser.add_argument("--afk", action="store_true",
                        help="Spawn with `--afk` (1.40.0+: auto-dismiss AskUserQuestion).")
    parser.add_argument("--prompt", default=DEFAULT_PROMPT,
                        help="Prompt to send (default forces a tool call).")
    parser.add_argument("--timeout", type=float, default=180.0,
                        help="Seconds to wait for the prompt to complete.")
    parser.add_argument("--out", type=Path, default=None,
                        help="Path to write raw JSONL of every JSON-RPC frame.")
    parser.add_argument("--allow-writes", action="store_true",
                        help="Allow fs/write_text_file requests to modify files.")
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
        afk=args.afk,
        prompt=args.prompt,
        timeout=args.timeout,
        raw_log_path=raw_log_path,
        quiet=args.quiet,
        allow_writes=args.allow_writes,
    )


if __name__ == "__main__":
    sys.exit(main())

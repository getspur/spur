#!/usr/bin/env python3
"""
Probe Codex/Kimi/Gemini ACP behavior for their native internal subagent mechanism.

This intentionally does not inject SPUR MCP delegation tools. It creates a
plain ACP session, prompts the agent to use its built-in subagent/worker/Task
capability, records every JSON-RPC frame, and summarizes the tool-call frames
that downstream renderers need to understand.

Usage:
  python3 scripts/probe_acp_subagents.py --agent kimi --yolo
  python3 scripts/probe_acp_subagents.py --agent codex
  python3 scripts/probe_acp_subagents.py --agent gemini
  python3 scripts/probe_acp_subagents.py --agent codex --out .spur/logs/codex-native-subagent.jsonl
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import signal
import struct
import subprocess
import sys
import threading
import time
import uuid
import zlib
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional


JsonRpcId = str | int
TERMINAL_DRAIN_TIMEOUT = 2.0

DEFAULT_PROMPT = (
    "Use your built-in internal subagent/worker capability exactly once. "
    "Spawn one internal subagent to inspect Cargo.toml in this workspace and "
    "report whether it contains a [workspace] section. The parent agent must "
    "not inspect Cargo.toml directly; the subagent must do the inspection. "
    "Do not edit files. After the subagent returns, summarize the subagent "
    "result in one sentence."
)

SUBAGENT_CLUE_WORDS = (
    "subagent",
    "sub-agent",
    "worker",
    "spawn_agent",
    "delegate",
    "task",
    "parenttooluseid",
    "parent_tool_use_id",
)

IMAGE_PROBE_PROMPT = (
    "I am attaching an image. Please describe the visual content in one sentence. "
    "If you do not see any image, say exactly: NO_IMAGE_RECEIVED."
)


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def utc_now_compact() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")


def new_request_id() -> str:
    return str(uuid.uuid4())


def make_request(id_: JsonRpcId, method: str, params: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def make_response(
    id_: JsonRpcId, result: Optional[dict] = None, error: Optional[dict] = None
) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    return msg


def png_chunk(kind: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    )


def synthesize_probe_png() -> bytes:
    width = 64
    height = 64
    rows = []
    for y in range(height):
        row = bytearray()
        for x in range(width):
            border = x in (0, width - 1) or y in (0, height - 1)
            cross = 28 <= x <= 35 or 28 <= y <= 35
            if border:
                row.extend((0, 0, 0))
            elif cross:
                row.extend((255, 255, 255))
            else:
                row.extend((255, 0, 0))
        rows.append(b"\x00" + bytes(row))

    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    idat = zlib.compress(b"".join(rows), level=9)
    return (
        signature
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", idat)
        + png_chunk(b"IEND", b"")
    )


def prompt_blocks(prompt: str, with_image: bool) -> list[dict]:
    blocks = [{"type": "text", "text": prompt}]
    if with_image:
        png = synthesize_probe_png()
        blocks.append(
            {
                "type": "image",
                "data": base64.b64encode(png).decode("ascii"),
                "mimeType": "image/png",
            }
        )
    return blocks


def probe_client_capabilities() -> dict:
    return {
        "fs": {"readTextFile": True, "writeTextFile": False},
        "terminal": True,
    }


def response_error_message(response: Optional[dict]) -> Optional[str]:
    if not response or "error" not in response:
        return None
    error = response.get("error") or {}
    code = error.get("code")
    message = error.get("message")
    if code is None and message is None:
        return json.dumps(error, sort_keys=True)
    if code is None:
        return str(message)
    if message is None:
        return f"code={code}"
    return f"code={code} {message}"


class ProbeRequestError(Exception):
    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


class TerminalState:
    def __init__(self, proc: subprocess.Popen, output_byte_limit: int):
        assert proc.stdout is not None and proc.stderr is not None
        self.proc = proc
        self.output_byte_limit = output_byte_limit
        self.output = ""
        self.truncated = False
        self._lock = threading.Lock()
        self._stdout = threading.Thread(
            target=self._read_stream, args=(proc.stdout,), daemon=True
        )
        self._stderr = threading.Thread(
            target=self._read_stream, args=(proc.stderr,), daemon=True
        )
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
            if len(self.output) > self.output_byte_limit:
                self.output = self.output[-self.output_byte_limit :]
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
        self.drain_reader_threads()
        return self.exit_status() or {}

    def drain_reader_threads(self) -> None:
        self._stdout.join(timeout=TERMINAL_DRAIN_TIMEOUT)
        self._stderr.join(timeout=TERMINAL_DRAIN_TIMEOUT)

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()


def optional_int(value: Any) -> Optional[int]:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def slice_text(text: str, line: Optional[int], limit: Optional[int]) -> str:
    lines = text.splitlines()
    start = max((line or 1) - 1, 0)
    selected = lines[start:]
    if limit is not None:
        selected = selected[: max(limit, 0)]
    return "\n".join(selected)


class AcpRequestHandlers:
    """Client-side handlers for requests that an ACP agent sends to SPUR."""

    def __init__(self, cwd: Path):
        self.cwd = cwd
        self.terminals: dict[str, TerminalState] = {}

    def resolve_path(self, raw_path: str) -> Path:
        path = Path(raw_path).expanduser()
        if path.is_absolute():
            return path
        return self.cwd.joinpath(path)

    def handle(self, req: dict) -> dict:
        method = req.get("method", "")
        params = req.get("params", {}) or {}
        if method == "fs/read_text_file":
            return self.read_text_file(params)
        if method == "terminal/create":
            return self.terminal_create(params)
        if method == "terminal/output":
            return self.get_terminal(params).output_response()
        if method == "terminal/wait_for_exit":
            return self.get_terminal(params).wait_response()
        if method == "terminal/kill":
            self.get_terminal(params).kill()
            return {}
        if method == "terminal/release":
            terminal_id = params.get("terminalId", params.get("terminal_id"))
            self.get_terminal(params).kill()
            self.terminals.pop(terminal_id, None)
            return {}
        if method == "fs/write_text_file":
            raise ProbeRequestError(-32603, "fs/write_text_file denied by native subagent probe")
        raise ProbeRequestError(-32601, f"probe does not implement {method}")

    def read_text_file(self, params: dict) -> dict:
        raw_path = params.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            raise ProbeRequestError(-32602, "fs/read_text_file requires path")
        path = self.resolve_path(raw_path)
        try:
            content = path.read_text()
        except Exception as exc:
            raise ProbeRequestError(-32603, f"Failed to read {path}: {exc}") from exc
        return {
            "content": slice_text(
                content,
                optional_int(params.get("line")),
                optional_int(params.get("limit")),
            )
        }

    def terminal_create(self, params: dict) -> dict:
        command = params.get("command")
        if not isinstance(command, str) or not command:
            raise ProbeRequestError(-32602, "terminal/create requires command")
        args = params.get("args") or []
        if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
            raise ProbeRequestError(-32602, "terminal/create args must be strings")
        cwd = self.resolve_path(params["cwd"]) if isinstance(params.get("cwd"), str) else self.cwd
        limit = optional_int(params.get("outputByteLimit", params.get("output_byte_limit")))
        if limit is None:
            limit = 1024 * 1024
        try:
            proc = subprocess.Popen(
                [command, *args],
                cwd=str(cwd),
                env=os.environ.copy(),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                errors="replace",
                bufsize=1,
            )
        except Exception as exc:
            raise ProbeRequestError(-32603, f"Failed to spawn {command!r}: {exc}") from exc
        terminal_id = str(uuid.uuid4())
        self.terminals[terminal_id] = TerminalState(proc, limit)
        return {"terminalId": terminal_id}

    def get_terminal(self, params: dict) -> TerminalState:
        terminal_id = params.get("terminalId", params.get("terminal_id"))
        if not isinstance(terminal_id, str) or terminal_id not in self.terminals:
            raise ProbeRequestError(-32602, f"Terminal {terminal_id!r} not found")
        return self.terminals[terminal_id]

    def close(self) -> None:
        for terminal in self.terminals.values():
            terminal.kill()


class AcpClient:
    def __init__(self, proc: subprocess.Popen, raw_log_path: Path, quiet: bool):
        assert proc.stdin is not None and proc.stdout is not None and proc.stderr is not None
        self.proc = proc
        self.stdin = proc.stdin
        self.stdout = proc.stdout
        self.stderr = proc.stderr
        self.responses: dict[JsonRpcId, dict] = {}
        self.notifications: list[dict] = []
        self.server_requests: list[dict] = []
        self.raw_log = open(raw_log_path, "w")
        self.quiet = quiet
        self._log_lock = threading.Lock()
        self._lock = threading.Lock()
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._stderr = threading.Thread(target=self._stderr_loop, daemon=True)
        self._reader.start()
        self._stderr.start()

    def _log(self, direction: str, obj: dict) -> None:
        with self._log_lock:
            self.raw_log.write(
                json.dumps({"ts": utc_now_iso(), "dir": direction, "msg": obj}) + "\n"
            )
            self.raw_log.flush()

    def _read_loop(self) -> None:
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
                    self.server_requests.append(obj)
                elif "method" in obj:
                    self.notifications.append(obj)
                elif "id" in obj:
                    self.responses[obj["id"]] = obj

    def _stderr_loop(self) -> None:
        for raw in self.stderr:
            line = raw.rstrip()
            self._log("stderr", {"text": line})
            if line and not self.quiet:
                print(f"[stderr] {line}", file=sys.stderr)

    def send(self, obj: dict) -> None:
        line = json.dumps(obj, separators=(",", ":"))
        self.stdin.write(line + "\n")
        self.stdin.flush()
        self._log("send", obj)
        if not self.quiet:
            print(f"[send] {line[:240]}")

    def wait_response(self, id_: JsonRpcId, timeout: float) -> Optional[dict]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self._lock:
                if id_ in self.responses:
                    return self.responses.pop(id_)
            time.sleep(0.02)
        return None

    def drain_notifications(self) -> list[dict]:
        with self._lock:
            items = self.notifications[:]
            self.notifications.clear()
            return items

    def drain_server_requests(self) -> list[dict]:
        with self._lock:
            items = self.server_requests[:]
            self.server_requests.clear()
            return items

    def pop_response(self, id_: JsonRpcId) -> Optional[dict]:
        with self._lock:
            return self.responses.pop(id_, None)

    def close(self) -> None:
        try:
            self.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        self._reader.join(timeout=1)
        self._stderr.join(timeout=1)
        self.raw_log.close()


def choose_permission_option(options: list[dict]) -> str:
    for pref in ("allow_always", "allow_once", "allow", "approve_for_session", "approve"):
        for opt in options:
            if opt.get("kind") == pref or opt.get("optionId") == pref:
                return opt.get("optionId") or opt.get("id") or pref
    if options:
        return options[0].get("optionId") or options[0].get("id") or "approve"
    return "approve"


def extract_tool_snapshot(notification: dict) -> Optional[dict]:
    update = notification.get("params", {}).get("update", {})
    variant = update.get("sessionUpdate")
    if variant not in {"tool_call", "tool_call_update"}:
        return None
    return {
        "variant": variant,
        "toolCallId": update.get("toolCallId"),
        "title": update.get("title"),
        "kind": update.get("kind"),
        "status": update.get("status"),
        "rawInput": update.get("rawInput"),
        "rawOutput": update.get("rawOutput"),
        "_meta": update.get("_meta"),
        "content": update.get("content"),
    }


def extract_text_content(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "".join(extract_text_content(item) for item in value)
    if not isinstance(value, dict):
        return ""

    text = value.get("text")
    if isinstance(text, str):
        return text

    content = value.get("content")
    if content is not None:
        return extract_text_content(content)

    return ""


def extract_agent_message_text(notification: dict) -> str:
    update = notification.get("params", {}).get("update", {})
    if update.get("sessionUpdate") != "agent_message_chunk":
        return ""
    return extract_text_content(update.get("content"))


def verdict_excerpt(text: str) -> str:
    return " ".join(text.split())[:200]


def has_subagent_clue(value: Any) -> bool:
    text = json.dumps(value, sort_keys=True).lower()
    return any(word in text for word in SUBAGENT_CLUE_WORDS)


def agent_command(args: argparse.Namespace) -> list[str]:
    if args.agent == "kimi":
        cmd = ["kimi"]
        if args.yolo:
            cmd.append("-y")
        if args.afk:
            cmd.append("--afk")
        cmd.append("acp")
        return cmd

    if args.agent == "gemini":
        cmd = ["gemini", "--acp"]
        if args.gemini_skip_trust:
            cmd.append("--skip-trust")
        if args.gemini_no_sandbox:
            cmd.append("--no-sandbox")
        cmd.append("-y")
        if args.gemini_model:
            cmd.extend(["-m", args.gemini_model])
        return cmd

    if args.agent == "claude-code":
        return ["npx", "--yes", args.claude_code_package]

    cmd = ["npx", "--yes", args.codex_package]
    for override in args.codex_config:
        cmd.extend(["-c", override])
    return cmd


def record_notification(
    notif: dict,
    notification_counts: Counter[str],
    tool_snapshots: list[dict],
    subagent_clues: list[dict],
) -> None:
    update = notif.get("params", {}).get("update", {})
    variant = update.get("sessionUpdate", notif.get("method", "unknown"))
    notification_counts[variant] += 1
    snap = extract_tool_snapshot(notif)
    if snap:
        tool_snapshots.append(snap)
    if has_subagent_clue(update):
        subagent_clues.append(update)


def record_prompt_notification(
    notif: dict,
    notification_counts: Counter[str],
    tool_snapshots: list[dict],
    subagent_clues: list[dict],
    agent_text_chunks: list[str],
) -> None:
    record_notification(notif, notification_counts, tool_snapshots, subagent_clues)
    if text := extract_agent_message_text(notif):
        agent_text_chunks.append(text)


def run_probe(args: argparse.Namespace) -> int:
    out_path = args.out
    if out_path is None:
        Path(".spur/logs").mkdir(parents=True, exist_ok=True)
        out_path = Path(f".spur/logs/probe-{args.agent}-native-subagents-{utc_now_compact()}.jsonl")

    cmd = agent_command(args)
    print("=" * 72)
    print(f"ACP NATIVE SUBAGENT PROBE  agent={args.agent}  cmd={' '.join(cmd)}")
    print(f"raw_log={out_path}")
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
    client = AcpClient(proc, out_path, args.quiet)
    handlers = AcpRequestHandlers(Path(os.getcwd()))

    notification_counts: Counter[str] = Counter()
    tool_snapshots: list[dict] = []
    subagent_clues: list[dict] = []
    agent_text_chunks: list[str] = []
    server_request_counts: Counter[str] = Counter()
    prompt_resp: Optional[dict] = None
    exit_code = 0
    effective_prompt = (
        IMAGE_PROBE_PROMPT if args.with_image and args.prompt == DEFAULT_PROMPT else args.prompt
    )
    prompt_payload = prompt_blocks(effective_prompt, args.with_image)
    if args.with_image:
        image_block = prompt_payload[-1]
        print(
            "image_probe="
            f"png_bytes={len(synthesize_probe_png())} "
            f"base64_chars={len(image_block['data'])}"
        )

    try:
        init_id = new_request_id()
        client.send(
            make_request(
                init_id,
                "initialize",
                {
                    "protocolVersion": 1,
                    "clientInfo": {"name": "spur-native-subagent-probe", "version": "0.1.0"},
                    "clientCapabilities": probe_client_capabilities(),
                },
            )
        )
        init_resp = client.wait_response(init_id, args.init_timeout)
        if init_resp is None:
            print("[FAIL] initialize timed out", file=sys.stderr)
            return 1
        if err := response_error_message(init_resp):
            print(f"[FAIL] initialize error: {err}", file=sys.stderr)
            return 1
        print(f"[recv] initialize result: {json.dumps(init_resp.get('result', {}))[:500]}")

        if args.authenticate:
            auth_methods = init_resp.get("result", {}).get("authMethods") or []
            if auth_methods:
                method_id = auth_methods[0].get("id")
                auth_id = new_request_id()
                client.send(make_request(auth_id, "authenticate", {"methodId": method_id}))
                auth_resp = client.wait_response(auth_id, args.init_timeout)
                if err := response_error_message(auth_resp):
                    print(f"[FAIL] authenticate error: {err}", file=sys.stderr)
                    return 1
                if not args.quiet:
                    print(f"[recv] authenticate response: {json.dumps(auth_resp or {})[:300]}")

        session_req_id = str(uuid.uuid4())
        session_new_id = new_request_id()
        client.send(
            make_request(
                session_new_id,
                "session/new",
                {
                    "sessionId": session_req_id,
                    "cwd": os.getcwd(),
                    "mcpServers": [],
                },
            )
        )
        session_resp = client.wait_response(session_new_id, args.session_timeout)
        if session_resp is None:
            print("[FAIL] session/new timed out", file=sys.stderr)
            return 1
        if err := response_error_message(session_resp):
            print(f"[FAIL] session/new error: {err}", file=sys.stderr)
            return 1
        session_id = session_resp.get("result", {}).get("sessionId", session_req_id)
        print(f"[recv] session/new result: {json.dumps(session_resp.get('result', {}))[:500]}")

        time.sleep(0.5)
        for notif in client.drain_notifications():
            record_notification(notif, notification_counts, tool_snapshots, subagent_clues)

        prompt_id = new_request_id()
        client.send(
            make_request(
                prompt_id,
                "session/prompt",
                {
                    "sessionId": session_id,
                    "prompt": prompt_payload,
                },
            )
        )

        start = time.time()
        while time.time() - start < args.timeout:
            for req in client.drain_server_requests():
                method = req.get("method", "")
                server_request_counts[method] += 1
                rid = req.get("id")
                if rid is None:
                    continue
                if method == "session/request_permission":
                    options = req.get("params", {}).get("options", []) or []
                    chosen = choose_permission_option(options)
                    if not args.quiet:
                        print(f"[req#{rid}] permission -> {chosen}")
                    client.send(
                        make_response(
                            rid,
                            {"outcome": {"outcome": "selected", "optionId": chosen}},
                        )
                    )
                    continue
                try:
                    result = handlers.handle(req)
                except ProbeRequestError as exc:
                    if not args.quiet:
                        print(f"[req#{rid}] {method} -> error {exc.message}")
                    client.send(make_response(rid, error={"code": exc.code, "message": exc.message}))
                else:
                    if not args.quiet:
                        print(f"[req#{rid}] {method} -> ok")
                    client.send(make_response(rid, result=result))

            for notif in client.drain_notifications():
                record_prompt_notification(
                    notif,
                    notification_counts,
                    tool_snapshots,
                    subagent_clues,
                    agent_text_chunks,
                )
                snap = extract_tool_snapshot(notif)
                if snap and not args.quiet:
                    print(
                        "[tool] "
                        f"{snap['variant']} id={snap['toolCallId']!r} "
                        f"title={snap['title']!r} status={snap['status']!r} "
                        f"meta={snap['_meta']!r}"
                    )

            prompt_resp = client.pop_response(prompt_id)
            if prompt_resp is not None:
                break
            time.sleep(0.05)

        if prompt_resp is not None:
            drain_deadline = time.time() + TERMINAL_DRAIN_TIMEOUT
            while time.time() < drain_deadline:
                drained = client.drain_notifications()
                if not drained:
                    time.sleep(0.05)
                    continue
                for notif in drained:
                    record_prompt_notification(
                        notif,
                        notification_counts,
                        tool_snapshots,
                        subagent_clues,
                        agent_text_chunks,
                    )

        if prompt_resp is None:
            print(f"[WARN] session/prompt did not complete within {args.timeout}s", file=sys.stderr)
            exit_code = 1
        elif err := response_error_message(prompt_resp):
            print(f"[FAIL] session/prompt error: {err}", file=sys.stderr)
            exit_code = 1
        else:
            print(f"[recv] session/prompt result: {json.dumps(prompt_resp.get('result', {}))[:500]}")
    finally:
        handlers.close()
        client.close()

    print("\n" + "=" * 72)
    print("REPORT")
    print("=" * 72)
    print("session/update variants:")
    for variant, count in notification_counts.most_common():
        print(f"  - {variant:30s} x{count}")
    print("server requests:")
    if server_request_counts:
        for method, count in server_request_counts.most_common():
            print(f"  - {method:30s} x{count}")
    else:
        print("  (none)")
    print("tool snapshots:")
    if not tool_snapshots:
        print("  (none)")
    for snap in tool_snapshots:
        print(
            "  - "
            f"{snap['variant']:16s} "
            f"id={snap['toolCallId']!r} "
            f"title={snap['title']!r} "
            f"kind={snap['kind']!r} "
            f"status={snap['status']!r} "
            f"meta={json.dumps(snap['_meta'], sort_keys=True) if snap['_meta'] else None}"
        )
    print("subagent clue updates:")
    if not subagent_clues:
        print("  (none)")
    for update in subagent_clues[:12]:
        compact = json.dumps(update, sort_keys=True)
        print(f"  - {compact[:600]}")
    if len(subagent_clues) > 12:
        print(f"  ... {len(subagent_clues) - 12} more in raw log")
    if args.with_image:
        prompt_err = response_error_message(prompt_resp)
        if prompt_resp is None:
            prompt_status = f"timeout after {args.timeout}s"
        elif prompt_err:
            prompt_status = prompt_err
        else:
            prompt_status = "ok"
        agent_text = "".join(agent_text_chunks)
        said_no_image = "NO_IMAGE_RECEIVED" in agent_text
        print(
            "IMAGE_VERDICT: "
            f"agent={args.agent} "
            f"session_prompt={prompt_status} "
            f"agent_said_NO_IMAGE_RECEIVED={str(said_no_image).lower()} "
            f"excerpt={verdict_excerpt(agent_text)!r}"
        )
    print(f"raw_log={out_path}")
    return exit_code


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--agent",
        choices=["codex", "kimi", "gemini", "claude-code"],
        default="kimi",
    )
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument(
        "--with-image",
        action="store_true",
        help="Send a small synthesized PNG image block after the prompt text.",
    )
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--init-timeout", type=float, default=30.0)
    parser.add_argument("--session-timeout", type=float, default=45.0)
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--yolo", "-y", action="store_true", help="Kimi only: pass -y")
    parser.add_argument("--afk", action="store_true", help="Kimi only: pass --afk")
    parser.add_argument(
        "--authenticate",
        action="store_true",
        help="Authenticate with the first advertised auth method before session/new.",
    )
    parser.add_argument(
        "--codex-package",
        default="@zed-industries/codex-acp@0.12.0",
        help="Codex ACP npm package passed to npx.",
    )
    parser.add_argument(
        "--claude-code-package",
        default="@agentclientprotocol/claude-agent-acp@0.30.0",
        help="claude-code ACP npm package passed to npx.",
    )
    parser.add_argument(
        "--codex-config",
        action="append",
        default=[],
        help="Codex ACP -c key=value override; repeatable.",
    )
    parser.add_argument(
        "--gemini-model",
        default="deep-thinker",
        help="Gemini model passed with -m; empty string omits -m.",
    )
    parser.add_argument(
        "--gemini-skip-trust",
        action="store_true",
        help="Gemini only: pass --skip-trust to bypass workspace trust setup.",
    )
    parser.add_argument(
        "--gemini-no-sandbox",
        action="store_true",
        help="Gemini only: pass --no-sandbox so ACP stdin is not consumed by sandbox relaunch.",
    )
    args = parser.parse_args()
    return run_probe(args)


if __name__ == "__main__":
    sys.exit(main())

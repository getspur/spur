"""Notebook MCP socket client -- Python sibling of the TS callTool/wire pair.

Wire contract (see sdk/typescript/src/wire.ts + call_tool.ts, and
sdk/docs/typescript-sdk.md): 4-byte big-endian length-prefixed JSON-RPC
frames over the unix socket at SPUR_NOTEBOOK_MCP_SOCKET. Handshake:
initialize (id=1) -> response -> notifications/initialized -> tools/call
(id=2) -> response. stdlib-only.
"""
from __future__ import annotations

import json
import os
import socket
import struct
from typing import Any

from .errors import EnvVarRequiredError, SpurAppError

_PROTOCOL_VERSION = "2025-11-25"
_CLIENT_INFO = {"name": "spur-app", "version": "0.1.0"}
_SOCKET_ENV = "SPUR_NOTEBOOK_MCP_SOCKET"


def _read_exactly(conn: socket.socket, size: int) -> bytes:
    buffer = b""
    while len(buffer) < size:
        chunk = conn.recv(size - len(buffer))
        if not chunk:
            raise SpurAppError("notebook MCP socket closed")
        buffer += chunk
    return buffer


def _read_frame(conn: socket.socket) -> dict[str, Any]:
    (length,) = struct.unpack(">I", _read_exactly(conn, 4))
    return json.loads(_read_exactly(conn, length))


def _write_frame(conn: socket.socket, value: dict[str, Any]) -> None:
    payload = json.dumps(value).encode()
    conn.sendall(struct.pack(">I", len(payload)) + payload)


def _raise_on_rpc_error(response: dict[str, Any], context: str) -> None:
    error = response.get("error")
    if error is not None:
        message = error.get("message") if isinstance(error, dict) else None
        raise SpurAppError(message or f"{context} error: {error!r}")


def _unwrap(result: Any) -> Any:
    if not isinstance(result, dict):
        return result
    if "structuredContent" in result:
        return result["structuredContent"]
    if "structured_content" in result:
        return result["structured_content"]
    for item in result.get("content") or []:
        if isinstance(item, dict) and item.get("type") == "text":
            text = item.get("text")
            if isinstance(text, str):
                try:
                    return json.loads(text)
                except ValueError:
                    return {"text": text}
    return result


class NotebookClient:
    """Call foundation notebook MCP tools from an app plugin server."""

    def __init__(self, socket_path: str | None = None) -> None:
        self._socket_path = socket_path

    def _resolve_socket_path(self) -> str:
        path = self._socket_path or os.environ.get(_SOCKET_ENV)
        if not path:
            raise EnvVarRequiredError(_SOCKET_ENV)
        return path

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            conn.connect(self._resolve_socket_path())
            _write_frame(
                conn,
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": _PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": _CLIENT_INFO,
                    },
                },
            )
            _raise_on_rpc_error(_read_frame(conn), "initialize")
            _write_frame(
                conn,
                {
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {},
                },
            )
            _write_frame(
                conn,
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": name, "arguments": arguments},
                },
            )
            response = _read_frame(conn)
            _raise_on_rpc_error(response, "tools/call")
            result = response.get("result") or {}
            return _unwrap(result)
        finally:
            conn.close()

    def push_source(self, port: str, ipc_bytes: bytes) -> Any:
        """Push Arrow IPC bytes into a declared notebook source port."""
        return self.call_tool(
            "notebook_push_source",
            {"port": port, "payload": list(ipc_bytes)},
        )

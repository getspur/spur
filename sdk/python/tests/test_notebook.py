"""Tests for spur_app.notebook -- real AF_UNIX socket, framed JSON-RPC."""
import json
import socket
import struct
import tempfile
import threading
from pathlib import Path

import pytest

from spur_app import EnvVarRequiredError, SpurAppError
from spur_app.notebook import NotebookClient


def _read_frame(conn):
    header = b""
    while len(header) < 4:
        chunk = conn.recv(4 - len(header))
        assert chunk, "client closed mid-header"
        header += chunk
    (length,) = struct.unpack(">I", header)
    body = b""
    while len(body) < length:
        chunk = conn.recv(length - len(body))
        assert chunk, "client closed mid-body"
        body += chunk
    return json.loads(body)


def _write_frame(conn, value):
    payload = json.dumps(value).encode()
    conn.sendall(struct.pack(">I", len(payload)) + payload)


class FakeNotebookSocket:
    """One-shot fake notebook MCP server on a real unix socket."""

    def __init__(self, tmp_path: Path, tool_result):
        self._tmpdir = tempfile.TemporaryDirectory(
            prefix=f"{tmp_path.name[:8]}-", dir="/tmp"
        )
        self.path = str(Path(self._tmpdir.name) / "nb.sock")
        self.requests = []
        self._tool_result = tool_result
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(self.path)
        self._server.listen(1)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self):
        conn, _ = self._server.accept()
        with conn:
            init = _read_frame(conn)
            self.requests.append(init)
            _write_frame(
                conn,
                {
                    "jsonrpc": "2.0",
                    "id": init["id"],
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "fake", "version": "0"},
                    },
                },
            )
            self.requests.append(_read_frame(conn))  # notifications/initialized
            call = _read_frame(conn)
            self.requests.append(call)
            _write_frame(
                conn,
                {"jsonrpc": "2.0", "id": call["id"], "result": self._tool_result},
            )

    def join(self):
        self._thread.join(timeout=5)
        self._server.close()
        self._tmpdir.cleanup()


def test_call_tool_handshake_and_structured_content(tmp_path, monkeypatch):
    fake = FakeNotebookSocket(tmp_path, {"structuredContent": {"ok": True, "n": 3}})
    monkeypatch.setenv("SPUR_NOTEBOOK_MCP_SOCKET", fake.path)
    result = NotebookClient().call_tool("wb_ping", {"x": 1})
    fake.join()
    assert result == {"ok": True, "n": 3}
    assert fake.requests[0]["method"] == "initialize"
    assert fake.requests[0]["params"]["protocolVersion"] == "2025-11-25"
    assert fake.requests[1]["method"] == "notifications/initialized"
    assert "id" not in fake.requests[1]
    assert fake.requests[2]["method"] == "tools/call"
    assert fake.requests[2]["params"] == {"name": "wb_ping", "arguments": {"x": 1}}


def test_call_tool_unwraps_text_content_json(tmp_path, monkeypatch):
    fake = FakeNotebookSocket(
        tmp_path, {"content": [{"type": "text", "text": json.dumps({"rows": 2})}]}
    )
    monkeypatch.setenv("SPUR_NOTEBOOK_MCP_SOCKET", fake.path)
    assert NotebookClient().call_tool("t", {}) == {"rows": 2}
    fake.join()


def test_push_source_sends_byte_array_payload(tmp_path, monkeypatch):
    fake = FakeNotebookSocket(tmp_path, {"structuredContent": {"ok": True}})
    monkeypatch.setenv("SPUR_NOTEBOOK_MCP_SOCKET", fake.path)
    NotebookClient().push_source("subgraph", b"\x01\x02\xff")
    fake.join()
    call = fake.requests[2]["params"]
    assert call["name"] == "notebook_push_source"
    assert call["arguments"] == {"port": "subgraph", "payload": [1, 2, 255]}


def test_missing_env_raises_env_var_required(monkeypatch):
    monkeypatch.delenv("SPUR_NOTEBOOK_MCP_SOCKET", raising=False)
    with pytest.raises(EnvVarRequiredError):
        NotebookClient().call_tool("t", {})


def test_rpc_error_raises_spur_app_error(tmp_path, monkeypatch):
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    tmpdir = tempfile.TemporaryDirectory(prefix=f"{tmp_path.name[:8]}-", dir="/tmp")
    path = str(Path(tmpdir.name) / "err.sock")
    server.bind(path)
    server.listen(1)

    def serve():
        conn, _ = server.accept()
        with conn:
            init = _read_frame(conn)
            _write_frame(
                conn,
                {
                    "jsonrpc": "2.0",
                    "id": init["id"],
                    "error": {"code": -32600, "message": "nope"},
                },
            )

    threading.Thread(target=serve, daemon=True).start()
    monkeypatch.setenv("SPUR_NOTEBOOK_MCP_SOCKET", path)
    with pytest.raises(SpurAppError, match="nope"):
        NotebookClient().call_tool("t", {})
    server.close()
    tmpdir.cleanup()

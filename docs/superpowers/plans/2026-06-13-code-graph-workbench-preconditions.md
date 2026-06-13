# Code Graph Workbench Preconditions Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-09-code-graph-workbench-app-design.md`
**Grounding evaluation:** session 2026-06-13 — all §2 transport primitives verified at HEAD; two
delivery blockers and two staleness items found. This epic clears the blockers and seeds the app
package; a follow-up epic implements the `wb_*` evidence tools and widgets.

**Goal:** Make the Code Graph Workbench buildable as specced: a Python notebook-socket SDK client,
`.spurapp` packer parity for full apps, a doctor HARD-GATE that polices app plugin tool names,
a refreshed spec, and the scaffolded app package seed.

**Architecture:** Five independent tasks (wide DAG, all parallel). Two extend the platform
(`spur_app` Python SDK; `export_spur_app` packer), one hardens the doctor, one refreshes the spec
doc, one creates the `app_gallery/code-graph-workbench/` seed mirroring `notebook_app_init`'s
`minimal` template output.

**Tech Stack:** Rust (serde, tokio), Python 3.11+ stdlib (socket/struct/json) + pytest, JSON.

**Build commands (mandatory):** Rust via `SPUR_REMOTE=0 scripts/spur-cargo test -p <crate> --lib`
(remote VM builds of spur-notebook are broken — bd-hsy2; never bare `cargo`). Python SDK tests via
`cd sdk/python && uv run --with pytest pytest tests/ -q`.

---

### Task 1: `spur_app.notebook` — Python notebook-socket client

**Task ID:** `t1-sdk-notebook-client`

**Files:**
- Create: `sdk/python/src/spur_app/notebook.py`
- Modify: `sdk/python/src/spur_app/__init__.py` (export `NotebookClient`)
- Test: `sdk/python/tests/test_notebook.py`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `NotebookClient().call_tool(name, args)` speaks the exact wire contract of
  `sdk/typescript/src/call_tool.ts` + `wire.ts`: unix socket from `SPUR_NOTEBOOK_MCP_SOCKET`,
  4-byte big-endian length-prefixed JSON frames, `initialize` (id=1, protocolVersion
  `"2025-11-25"`) → response → `notifications/initialized` → `tools/call` (id=2) → response,
  unwrapping `structuredContent` → `structured_content` → `content[].text` (JSON-parsed) → result.
- [ ] `push_source(port, ipc_bytes)` calls `notebook_push_source` with
  `{"port": port, "payload": list(ipc_bytes)}` (the foundation tool takes a JSON byte array).
- [ ] Missing env var raises `EnvVarRequiredError("SPUR_NOTEBOOK_MCP_SOCKET")`; JSON-RPC `error`
  responses raise `SpurAppError` with the server message.
- [ ] stdlib-only (socket, struct, json, os); tests use a real `AF_UNIX` listener in a thread —
  no protocol mocks.
- [ ] `cd sdk/python && uv run --with pytest pytest tests/ -q` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above.
- OUT of scope: `sdk/typescript/**`, `app.py`, `ports.py`, any Rust code, `sdk/fixtures/**`.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing test** (`sdk/python/tests/test_notebook.py`):

```python
"""Tests for spur_app.notebook — real AF_UNIX socket, framed JSON-RPC."""
import json
import socket
import struct
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
        self.path = str(tmp_path / "nb.sock")
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
            _write_frame(conn, {"jsonrpc": "2.0", "id": init["id"], "result": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "serverInfo": {"name": "fake", "version": "0"}}})
            self.requests.append(_read_frame(conn))  # notifications/initialized
            call = _read_frame(conn)
            self.requests.append(call)
            _write_frame(conn, {"jsonrpc": "2.0", "id": call["id"],
                                "result": self._tool_result})

    def join(self):
        self._thread.join(timeout=5)


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
        tmp_path, {"content": [{"type": "text", "text": json.dumps({"rows": 2})}]})
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
    path = str(tmp_path / "err.sock")
    server.bind(path)
    server.listen(1)

    def serve():
        conn, _ = server.accept()
        with conn:
            init = _read_frame(conn)
            _write_frame(conn, {"jsonrpc": "2.0", "id": init["id"],
                                "error": {"code": -32600, "message": "nope"}})

    threading.Thread(target=serve, daemon=True).start()
    monkeypatch.setenv("SPUR_NOTEBOOK_MCP_SOCKET", path)
    with pytest.raises(SpurAppError, match="nope"):
        NotebookClient().call_tool("t", {})
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd sdk/python && uv run --with pytest pytest tests/test_notebook.py -q`
Expected: FAIL — `ModuleNotFoundError: spur_app.notebook`.

- [ ] **Step 3: Implement** `sdk/python/src/spur_app/notebook.py`:

```python
"""Notebook MCP socket client — Python sibling of the TS callTool/wire pair.

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


def _unwrap(result: dict[str, Any]) -> Any:
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
            raise EnvVarRequiredError(
                _SOCKET_ENV,
                f"{_SOCKET_ENV} not provisioned — the host injects it at "
                "plugin spawn; run inside app mode",
            )
        return path

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            conn.connect(self._resolve_socket_path())
            _write_frame(conn, {
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": _PROTOCOL_VERSION,
                           "capabilities": {}, "clientInfo": _CLIENT_INFO},
            })
            _raise_on_rpc_error(_read_frame(conn), "initialize")
            _write_frame(conn, {"jsonrpc": "2.0",
                                "method": "notifications/initialized",
                                "params": {}})
            _write_frame(conn, {
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            })
            response = _read_frame(conn)
            _raise_on_rpc_error(response, "tools/call")
            return _unwrap(response.get("result") or {})
        finally:
            conn.close()

    def push_source(self, port: str, ipc_bytes: bytes) -> Any:
        """Push Arrow IPC bytes into a declared notebook source port."""
        return self.call_tool(
            "notebook_push_source",
            {"port": port, "payload": list(ipc_bytes)},
        )
```

Check the exact `EnvVarRequiredError.__init__` signature in `sdk/python/src/spur_app/errors.py`
first and match it (it may take `(env_var, message)` or a single message — adapt the call,
keeping the env-var name in the message).

- [ ] **Step 4: Export it** — in `sdk/python/src/spur_app/__init__.py` add
  `from .notebook import NotebookClient` and `"NotebookClient"` to `__all__`.

- [ ] **Step 5: Run to verify pass**

Run: `cd sdk/python && uv run --with pytest pytest tests/ -q`
Expected: all tests PASS (existing suites stay green).

- [ ] **Step 6: Commit**

```bash
git add sdk/python/src/spur_app/notebook.py sdk/python/src/spur_app/__init__.py sdk/python/tests/test_notebook.py
git commit -m "feat(sdk): add spur_app NotebookClient socket client"
```

---

### Task 2: `.spurapp` packer parity — bundle the authored app

**Task ID:** `t2-packer-parity`

**Files:**
- Modify: `crates/spur-notebook/src/spur_app.rs` (`export_spur_app`, ~lines 210–253; helpers below it)
- Test: same file `#[cfg(test)] mod tests` (or extend `crates/spur-notebook/tests/` if a packer
  round-trip test already lives there — check first with `rg -l export_spur_app crates/spur-notebook/tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] When `spur-app.json` exists next to the source notebook, `export_spur_app`:
  parses it as `SpurAppManifest` (fail with `InvalidManifestJson` on parse error — never silently
  synthesize over an authored manifest), keeps its declared fields, and overlays only the
  packer-computed `widgets`, `dependencies`, and `ports` fields onto it.
- [ ] Bundles into the archive: the parent directory of `mcp_server.entry` recursively (e.g. all of
  `server/`), the `skill` file (default `skill/SKILL.md` when the field is None but the file
  exists), and the `sdk.typescript` directory — preserving archive-relative paths; skipping
  `__pycache__`, `.pytest_cache`, and hidden files.
- [ ] No authored manifest → behavior identical to today (synthesized minimal manifest; existing
  tests untouched and green).
- [ ] Round-trip test: build a temp app dir (authored manifest with `mcp_server` + `skill` + `sdk`),
  export, then `archive::read_manifest` returns the authored name/capabilities and the archive
  lists `server/main.py`, `skill/SKILL.md`, `sdk/call_tool.ts`.
- [ ] `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib spur_app` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `crates/spur-notebook/src/spur_app.rs` (+ its tests).
- OUT of scope: `spur_app/archive.rs` wire format, `spur_app/scaffold.rs`,
  `mcp/tools/export_spur_app.rs` (its parameter surface must not change), `import_spur_app`.
- If the archive module needs new entry-validation behavior, emit `scope_drift` instead of
  editing `archive.rs`.

**Implementation:**
- [ ] **Step 1: Write the failing test** in `spur_app.rs` tests mod:

```rust
#[test]
fn export_bundles_authored_manifest_server_skill_and_sdk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("my-app");
    std::fs::create_dir_all(root.join("server")).unwrap();
    std::fs::create_dir_all(root.join("skill")).unwrap();
    std::fs::create_dir_all(root.join("sdk")).unwrap();
    std::fs::write(root.join("app.ipynb"), b"{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":5}").unwrap();
    std::fs::write(root.join("server/main.py"), b"print('hi')\n").unwrap();
    std::fs::write(root.join("server/requirements.txt"), b"mcp>=1.0.0\n").unwrap();
    std::fs::write(root.join("skill/SKILL.md"), b"# skill\n").unwrap();
    std::fs::write(root.join("sdk/call_tool.ts"), b"// vendored\n").unwrap();

    let mut manifest = SpurAppManifest::minimal("authored-app", SPUR_APP_ENTRY_NOTEBOOK);
    manifest.mcp_server = Some(SpurAppMcpServer {
        server_type: "python".into(),
        entry: "server/main.py".into(),
        requirements: Some("server/requirements.txt".into()),
        env: Default::default(),
    });
    manifest.skill = Some("skill/SKILL.md".into());
    manifest.sdk = Some(SpurAppSdk { typescript: Some("sdk".into()) });
    std::fs::write(
        root.join(SPUR_APP_MANIFEST),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    ).unwrap();

    let out = tmp.path().join("out.spurapp");
    let exported = export_spur_app(SpurAppExportOptions {
        notebook_path: root.join("app.ipynb"),
        output_path: out.clone(),
        name: None,
        widget_assets: vec![],
        include_port_snapshots: false,
        dependency_roots: vec![root.clone()],
    }).expect("export");

    let read = archive::read_manifest(std::fs::File::open(&exported.output_path).unwrap())
        .expect("manifest");
    assert_eq!(read.name, "authored-app", "authored manifest must win");
    assert!(read.mcp_server.is_some());

    let names = archive::list_entry_names(std::fs::File::open(&out).unwrap()).expect("entries");
    for expected in ["server/main.py", "server/requirements.txt", "skill/SKILL.md", "sdk/call_tool.ts"] {
        assert!(names.iter().any(|n| n == expected), "missing {expected}: {names:?}");
    }
}
```

If `archive::list_entry_names` does not exist, check `archive.rs` for an existing entry-listing
helper (e.g. used by import) and use that; only if none exists, read the zip in the test directly
with the `zip` crate already in the dependency tree (`zip::ZipArchive::new` → `file_names()`).
Do NOT add a new public helper to `archive.rs` (out of scope).

- [ ] **Step 2: Run to verify it fails**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib export_bundles_authored`
Expected: FAIL — manifest read returns `"my-app"`/no mcp_server and archive lacks `server/main.py`.

- [ ] **Step 3: Implement.** In `export_spur_app`, after building `name`:

```rust
    let app_root = options
        .notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let authored_path = app_root.join(SPUR_APP_MANIFEST);
    let mut manifest = if authored_path.is_file() {
        let raw = fs::read(&authored_path)?;
        serde_json::from_slice::<SpurAppManifest>(&raw)
            .map_err(archive::SpurAppArchiveError::InvalidManifestJson)?
    } else {
        SpurAppManifest::minimal(name, SPUR_APP_ENTRY_NOTEBOOK)
    };
```

then keep the existing `manifest.widgets = ...` / `manifest.dependencies = ...` /
port-snapshot lines unchanged (they overlay the packer-computed fields), and before writing the
manifest entry add:

```rust
    collect_app_files(&app_root, &manifest, &mut entries)?;
```

with the helper:

```rust
/// Bundle the authored app files referenced by the manifest: the server entry's
/// directory (recursively), the skill file, and the vendored SDK directory.
fn collect_app_files(
    app_root: &Path,
    manifest: &SpurAppManifest,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(server) = &manifest.mcp_server {
        if let Some(dir) = Path::new(&server.entry).parent() {
            if !dir.as_os_str().is_empty() {
                roots.push(dir.to_path_buf());
            }
        }
    }
    if let Some(sdk) = manifest.sdk.as_ref().and_then(|s| s.typescript.as_deref()) {
        roots.push(PathBuf::from(sdk));
    }
    for rel_dir in roots {
        collect_dir_recursive(app_root, &rel_dir, entries)?;
    }
    let skill_rel = manifest.skill.as_deref().unwrap_or("skill/SKILL.md");
    let skill_abs = app_root.join(skill_rel);
    if skill_abs.is_file() {
        push_entry_once(entries, skill_rel.to_string(), fs::read(&skill_abs)?);
    }
    Ok(())
}

fn collect_dir_recursive(
    app_root: &Path,
    rel_dir: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    let abs = app_root.join(rel_dir);
    if !abs.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&abs)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with('.') || name == "__pycache__" || name == ".pytest_cache" {
            continue;
        }
        let rel = rel_dir.join(&file_name);
        if entry.file_type()?.is_dir() {
            collect_dir_recursive(app_root, &rel, entries)?;
        } else {
            // Archive paths are forward-slash joined, app-root-relative.
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            push_entry_once(entries, key, fs::read(entry.path())?);
        }
    }
    Ok(())
}

fn push_entry_once(entries: &mut Vec<(String, Vec<u8>)>, key: String, bytes: Vec<u8>) {
    if !entries.iter().any(|(existing, _)| existing == &key) {
        entries.push((key, bytes));
    }
}
```

Note: dependency lock files are already collected by `collect_dependency_locks` into `env/` —
`push_entry_once` prevents a duplicate-path archive error when `server/requirements.txt` is also
referenced as a dependency lock. Verify against `DEPENDENCY_LOCK_FILES` handling; if locks are
stored under an `env/` prefix there is no collision and `push_entry_once` is still correct.
The `name` variable becomes unused in the authored branch — apply `options.name` override onto
the authored manifest only when `options.name` is `Some` (explicit override wins), otherwise keep
the authored `name`.

- [ ] **Step 4: Run to verify pass + no regressions**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib spur_app`
Expected: new test PASS; all existing `spur_app`/scaffold/export tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/spur_app.rs
git commit -m "feat(spur-notebook): pack authored manifest, server, skill, sdk into .spurapp"
```

**Scope Drift Checkpoint:** if export/import symmetry forces changes in `import_spur_app` or
`archive.rs`, emit `scope_drift` with the file list — do not expand silently.

---

### Task 3: doctor HARD-GATE polices app plugin tool names

**Task ID:** `t3-doctor-plugin-gate`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs` (`check_skill`, ~lines 567–657)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A HARD-GATE tool name whose prefix (text through the first `_`, e.g. `wb_`) matches the
  prefix of ANY tool in the spawned plugin's `tools/list` is checkable: pass when present in the
  live surface, **fail** when absent (today such names are silently skipped).
- [ ] Names matching neither the hardcoded prefixes nor any plugin-tool prefix remain skipped
  (no false positives on backticked words like `spur.put` or env-var names).
- [ ] When plugin spawn was skipped, behavior is unchanged (warn-level only).
- [ ] Unit tests cover: phantom `wb_` name fails when plugin exposes other `wb_*` tools; `wb_`
  name passes when in plugin list; unknown-prefix word still skipped.
- [ ] `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib notebook_app_doctor` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `notebook_app_doctor.rs` only.
- OUT of scope: plugin_loader, scaffold, any skill files.

**Implementation:**
- [ ] **Step 1: Write the failing tests** (in a `#[cfg(test)] mod tests` in the same file — add it
  if absent; `extract_hard_gate_tools` and the new prefix helper are testable without spawning):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_tool_prefixes_derive_from_tools_list() {
        let tools = vec!["wb_subgraph".to_string(), "wb_scorecard".to_string()];
        let prefixes = plugin_tool_prefixes(&tools);
        assert!(prefixes.contains("wb_"));
        assert_eq!(prefixes.len(), 1);
    }

    #[test]
    fn gate_name_checkable_when_prefix_matches_plugin_tools() {
        let plugin_tools = vec!["wb_subgraph".to_string()];
        let prefixes = plugin_tool_prefixes(&plugin_tools);
        assert!(is_checkable_gate_name("wb_blast_radius", &prefixes));
        assert!(is_checkable_gate_name("notebook_run_cell", &prefixes));
        assert!(!is_checkable_gate_name("spur.put", &prefixes));
        assert!(!is_checkable_gate_name("SPUR_PORTS_ROOT", &prefixes));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib notebook_app_doctor`
Expected: compile FAIL — `plugin_tool_prefixes` / `is_checkable_gate_name` undefined.

- [ ] **Step 3: Implement.** Add above `check_skill`:

```rust
/// Prefixes (through the first '_') of the plugin's live tool names, e.g.
/// {"wb_"} for a plugin exposing wb_subgraph/wb_scorecard. A gate name with a
/// matching prefix is checkable against the live surface, so a renamed or
/// phantom app tool fails the doctor instead of being silently skipped.
fn plugin_tool_prefixes(plugin_tools: &[String]) -> std::collections::HashSet<String> {
    plugin_tools
        .iter()
        .filter_map(|tool| tool.split_once('_').map(|(p, _)| format!("{p}_")))
        .collect()
}

fn is_checkable_gate_name(
    tool: &str,
    plugin_prefixes: &std::collections::HashSet<String>,
) -> bool {
    const KNOWN_PREFIXES: [&str; 3] = ["notebook_", "notebook.", "html_video_"];
    KNOWN_PREFIXES.iter().any(|p| tool.starts_with(p))
        || plugin_prefixes.iter().any(|p| tool.starts_with(p.as_str()))
}
```

In `check_skill`, replace the `known_prefixes` array and the
`starts_with_known_prefix` check with:

```rust
    let plugin_prefixes = plugin_tool_prefixes(plugin_tools);
    for tool in &gate_tools {
        if !is_checkable_gate_name(tool, &plugin_prefixes) {
            continue;
        }
        // existing in_notebook / in_plugin / spawn_skipped logic unchanged
```

- [ ] **Step 4: Run to verify pass**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib notebook_app_doctor`
Expected: PASS. Also run `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-notebook --lib scaffold init_`
to confirm the scaffolded-app doctor-green test still passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs
git commit -m "fix(spur-notebook): doctor gate checks app plugin tool prefixes"
```

---

### Task 4: refresh the workbench spec against verified HEAD

**Task ID:** `t4-spec-refresh`

**Files:**
- Modify: `docs/superpowers/specs/2026-06-09-code-graph-workbench-app-design.md`

**Depends on:** none

**Acceptance Criteria:**
- [ ] §2 analyst table: replace the `onager_edges` row with
  `| Co-change graph | v_file_cochange | file_a, file_b, cochange_count, has_static_edge |`
  (verified live: no onager table exists; temporal data surfaces only as `_meta.temporal_edge_count`).
- [ ] §4 manifest comment block: drop `runtime.features [...]` (deprecated — doctor warns);
  declare `capabilities { "active_output_scripts": true }`, `skill "skill/SKILL.md"`, and
  `sdk { "typescript": "sdk" }`; package tree gains `conftest.py`, `tests/`, and `sdk/`
  (`call_tool.ts`, `wire.ts`) matching the `notebook_app_init` minimal-template layout.
- [ ] §5 step 4: the push goes through `spur_app.notebook.NotebookClient.push_source(port,
  ipc_bytes)` (added in this epic) — no hand-written socket code in the app.
- [ ] §9 adds: "Run `notebook_app_doctor` (exists) before pack; packaging acceptance depends on
  the packer-parity precondition (authored manifest + `server/` + `skill/` + `sdk/` bundled)."
- [ ] §10 Task 0 marked complete with date 2026-06-13 (drift check executed; all §2 primitives
  verified; AppScope lives in `sidebar_chat/types.rs`); a "Preconditions (epic'd)" list names the
  four work items of this plan.
- [ ] §8 adds one line: the Deno kernel runs config-free (`DENO_NO_PACKAGE_JSON=1` in the bundled
  kernelspec), so installed apps under `~/.spur/apps/` are immune to ancestor `package.json`
  resolution breakage.
- [ ] No other sections reworded; diff stays surgical.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the one spec file.
- OUT of scope: every other doc, all code.

**Implementation:**
- [ ] **Step 1:** Apply the seven edits above as minimal diffs (each acceptance bullet is one edit
  site; the §2 row, §4 block, and §5 sentence are replacements; §8/§9/§10 are insertions).
- [ ] **Step 2:** `rg -n "onager|runtime.features" docs/superpowers/specs/2026-06-09-code-graph-workbench-app-design.md`
  Expected: no `onager` hits; `features` only in the line explaining its deprecation (if kept).
- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-06-09-code-graph-workbench-app-design.md
git commit -m "docs(specs): refresh code-graph-workbench spec to verified HEAD"
```

---

### Task 5: `app_gallery/code-graph-workbench/` package seed

**Task ID:** `t5-workbench-app-seed`

**Files:**
- Create: `app_gallery/code-graph-workbench/spur-app.json`
- Create: `app_gallery/code-graph-workbench/app.ipynb`
- Create: `app_gallery/code-graph-workbench/skill/SKILL.md`
- Create: `app_gallery/code-graph-workbench/server/main.py`
- Create: `app_gallery/code-graph-workbench/server/requirements.txt`
- Create: `app_gallery/code-graph-workbench/conftest.py`
- Create: `app_gallery/code-graph-workbench/tests/test_app.py`
- Create: `app_gallery/code-graph-workbench/sdk/call_tool.ts` + `sdk/wire.ts` (byte-identical
  copies: `cp sdk/typescript/src/call_tool.ts sdk/typescript/src/wire.ts app_gallery/code-graph-workbench/sdk/`)

**Depends on:** none (evidence tools land in the follow-up epic)

**Acceptance Criteria:**
- [ ] `spur-app.json` parses as `SpurAppManifest` (validate with
  `python3 -c "import json; json.load(open('app_gallery/code-graph-workbench/spur-app.json'))"`,
  and field names exactly match `crates/spur-notebook/src/spur_app.rs`).
- [ ] `app.ipynb` is nbformat 4.5 (every cell has an `id`) and declares the four evidence source
  ports from spec §4 (`subgraph`, `analyst_rows`, `scorecard`, `cochange`) on its widget
  placeholder cells via `metadata.spur.dag.source = {"kind": "frontend", "port": ...}`.
- [ ] `server/main.py` uses `from spur_app import App`; registers one smoke tool `wb_ping` that
  returns `{"ok": True, "app": "code-graph-workbench"}`. No protocol code.
- [ ] `cd app_gallery/code-graph-workbench && uv run --no-project --with pytest --with-requirements server/requirements.txt pytest tests/ -q` green.
- [ ] Vendored `sdk/*.ts` byte-identical to `sdk/typescript/src/*.ts` (`cmp` both files).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: files under `app_gallery/code-graph-workbench/` only.
- OUT of scope: `app_gallery/html_video/**`, `sdk/**` (copy FROM it, never edit it), all crates.

**Implementation:**
- [ ] **Step 1:** Write `spur-app.json`:

```json
{
  "schema": "spur.app/v1",
  "name": "code-graph-workbench",
  "entry_notebook": "app.ipynb",
  "open_mode": "app",
  "runtime": { "jute_min": "0.1.0", "features": [] },
  "widgets": [],
  "ports": null,
  "dependencies": { "python": "server/requirements.txt" },
  "mcp_server": {
    "type": "python",
    "entry": "server/main.py",
    "requirements": "server/requirements.txt",
    "env": {}
  },
  "capabilities": { "active_output_scripts": true },
  "skill": "skill/SKILL.md",
  "sdk": { "typescript": "sdk" }
}
```

- [ ] **Step 2:** Write `server/requirements.txt`:

```
mcp>=1.0.0
duckdb>=1.0.0
pyarrow>=17.0.0
# TODO(U7): replace with spur-app>=0.1.0 once published to PyPI
../../sdk/python
```

- [ ] **Step 3:** Write `server/main.py`:

```python
"""code-graph-workbench — evidence-tool MCP server (seed).

wb_* evidence tools (blast_radius, subgraph, scorecard, cochange) land in
the follow-up epic; this seed pins the app contract and test harness.
"""
from spur_app import App

app = App("code-graph-workbench")


@app.tool()
def wb_ping() -> dict:
    """Smoke tool: verifies the plugin surface is live."""
    return {"ok": True, "app": "code-graph-workbench"}


if __name__ == "__main__":
    app.run()
```

- [ ] **Step 4:** Write `app.ipynb` (exact JSON; one markdown cell + four `anywidget-afm`
  placeholder cells declaring the evidence source ports; each placeholder renders an honest
  "no evidence pushed yet" state):

```json
{
  "cells": [
    {
      "cell_type": "markdown",
      "id": "intro",
      "metadata": { "spur": { "version": 1 } },
      "source": [
        "# Code Graph Workbench\n",
        "\n",
        "Ask the AI sidebar about the codebase; evidence tools paint these panels.\n",
        "Panels: analyst (left), graph (center), inspector (bottom). Spec:\n",
        "`docs/superpowers/specs/2026-06-09-code-graph-workbench-app-design.md`."
      ]
    },
    {
      "cell_type": "code",
      "id": "wb-graph",
      "metadata": { "spur": { "version": 1, "code_type": "javascript", "dag": { "produces": [], "consumes": [], "source": { "kind": "frontend", "port": "subgraph" } }, "frontend": { "kind": "custom", "binds": ["subgraph", "scorecard"], "emits": [] } } },
      "execution_count": null,
      "outputs": [],
      "source": [
        "// wb-graph placeholder — bound to `subgraph` + `scorecard` evidence ports.\n",
        "Deno.jupyter.html`<div data-wb=\"graph\"><h3>Graph</h3><p>no evidence pushed this turn</p></div>`;"
      ]
    },
    {
      "cell_type": "code",
      "id": "wb-analyst",
      "metadata": { "spur": { "version": 1, "code_type": "javascript", "dag": { "produces": [], "consumes": [], "source": { "kind": "frontend", "port": "analyst_rows" } }, "frontend": { "kind": "custom", "binds": ["analyst_rows"], "emits": [] } } },
      "execution_count": null,
      "outputs": [],
      "source": [
        "// wb-analyst placeholder — bound to `analyst_rows` evidence port.\n",
        "Deno.jupyter.html`<div data-wb=\"analyst\"><h3>Analyst</h3><p>no evidence pushed this turn</p></div>`;"
      ]
    },
    {
      "cell_type": "code",
      "id": "wb-inspector",
      "metadata": { "spur": { "version": 1, "code_type": "javascript", "dag": { "produces": [], "consumes": [], "source": { "kind": "frontend", "port": "scorecard" } }, "frontend": { "kind": "custom", "binds": ["scorecard", "cochange"], "emits": [] } } },
      "execution_count": null,
      "outputs": [],
      "source": [
        "// wb-inspector placeholder — bound to `scorecard` + `cochange` evidence ports.\n",
        "Deno.jupyter.html`<div data-wb=\"inspector\"><h3>Inspector</h3><p>no evidence pushed this turn</p></div>`;"
      ]
    },
    {
      "cell_type": "code",
      "id": "wb-status",
      "metadata": { "spur": { "version": 1, "code_type": "javascript", "dag": { "produces": [], "consumes": [], "source": { "kind": "frontend", "port": "cochange" } }, "frontend": { "kind": "custom", "binds": [], "emits": [] } } },
      "execution_count": null,
      "outputs": [],
      "source": [
        "// wb-status placeholder — status rail; cochange source port declared here.\n",
        "Deno.jupyter.html`<div data-wb=\"status\"><p>workbench ready — ask the AI sidebar</p></div>`;"
      ]
    }
  ],
  "metadata": {},
  "nbformat": 4,
  "nbformat_minor": 5
}
```

- [ ] **Step 5:** Write `skill/SKILL.md`:

```markdown
---
name: code-graph-workbench
description: "Use when answering codebase questions inside the Code Graph Workbench app — call wb_* evidence tools before answering and cite pushed stable_symbol_ids."
---

# Code Graph Workbench — Evidence-Grounded Answers

<HARD-GATE>
Answer ONLY from evidence pushed this turn by MCP tools:
(`wb_ping`, `notebook_push_source`, `notebook_run_cell`).
Call the relevant wb_* evidence tool(s) BEFORE answering. Every claim in the
answer must cite a `stable_symbol_id` returned by a tool this turn. If no tool
was called, say "no evidence pushed this turn" instead of answering.
</HARD-GATE>

## The loop

1. Resolve what the user is asking about (symbol, file, or change).
2. Call the matching evidence tool — it runs the real analyst/graph query,
   pushes Arrow to the bound panel port, and returns the pushed
   `stable_symbol_id`s for your citations.
3. Answer compactly; map citation markers `[n1]`, `[n2]` to the returned ids.
4. Never fabricate counts, scores, or edges: if the tool returned an empty or
   guided-empty result, report that honestly.

(Evidence tools `wb_blast_radius`, `wb_subgraph`, `wb_scorecard`, `wb_cochange`
arrive in the follow-up epic; until then `wb_ping` verifies the surface.)
```

- [ ] **Step 6:** Write `conftest.py`:

```python
"""Shared pytest fixtures for code-graph-workbench (spur_app.testing re-export)."""
from spur_app.testing import fake_port_store  # noqa: F401
```

and `tests/test_app.py`:

```python
"""Tests for the code-graph-workbench server seed."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))

from main import wb_ping


def test_wb_ping_reports_live_surface():
    result = wb_ping()
    assert result == {"ok": True, "app": "code-graph-workbench"}
```

- [ ] **Step 7:** Vendor the SDK and verify:

```bash
mkdir -p app_gallery/code-graph-workbench/sdk
cp sdk/typescript/src/call_tool.ts sdk/typescript/src/wire.ts app_gallery/code-graph-workbench/sdk/
cmp sdk/typescript/src/call_tool.ts app_gallery/code-graph-workbench/sdk/call_tool.ts
cmp sdk/typescript/src/wire.ts app_gallery/code-graph-workbench/sdk/wire.ts
```

- [ ] **Step 8:** Run the tests:

Run: `cd app_gallery/code-graph-workbench && uv run --no-project --with pytest --with-requirements server/requirements.txt pytest tests/ -q`
Expected: PASS (1 test).

- [ ] **Step 9: Commit**

```bash
git add app_gallery/code-graph-workbench
git commit -m "feat(app-gallery): seed code-graph-workbench app package"
```

---

## Dependency DAG

```
t1-sdk-notebook-client   (root)
t2-packer-parity     (root)
t3-doctor-plugin-gate      (root)
t4-spec-refresh                 (root)
t5-workbench-app-seed           (root)
```

All five tasks are independent — maximum parallelism, no chains. The follow-up epic
(`wb_*` evidence tools + real widgets) depends on task-1 (NotebookClient), task-2 (packaging
acceptance), and task-5 (package seed) having merged.

## Self-Review (done)

- **Spec coverage:** this epic intentionally covers the *preconditions* (§10 Task 0 closure, §4/§2
  staleness, packaging §9 dependency, SDK seam for §5) plus Task 1 (app package). Spec Tasks 2–3
  (evidence tools, widgets) are the follow-up epic by design.
- **Placeholders:** none — every step carries exact file content or commands.
- **Type consistency:** `SpurAppSdk`/`SpurAppMcpServer` usage matches `spur_app.rs` at HEAD;
  `EnvVarRequiredError` signature flagged for verification in Task 1 Step 3.
- **DAG:** five roots, zero edges — valid, maximally parallel.
- **beads compatibility:** unique task IDs, explicit empty depends_on, verifiable acceptance
  criteria, explicit scope boundaries per task.

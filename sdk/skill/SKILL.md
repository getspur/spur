---
name: app-dev
description: "Use when building or extending a Spur notebook app: declaring capabilities, writing a Python MCP server, writing TypeScript frontend cells, running doctor, and packing for distribution."
role: both
---
<!-- SPUR-MANAGED v=1 skill=app-dev sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# App Dev — Spur Notebook App Development

You are building a Spur notebook app. A Spur app is a `.spurapp` bundle that
contains a Jupyter notebook, an optional Python MCP server, optional TypeScript
frontend cells, widget assets, and a `spur-app.json` manifest. The host opens
the bundle in *app mode*, where the output pane renders the notebook's HTML
outputs with full trust and the Python server is started as an MCP plugin.

<HARD-GATE>
Tool-name verification. Every tool listed here MUST either exist in the live
tool surface (verified at task write time against
`crates/spur-notebook/src/mcp/tools/`) or be explicitly marked PLANNED.

| Tool name                   | Status   | Path |
|-----------------------------|----------|------|
| `notebook_export_spur_app`  | EXISTS   | `crates/spur-notebook/src/mcp/tools/export_spur_app.rs` |
| `notebook_import_spur_app`  | EXISTS   | `crates/spur-notebook/src/mcp/tools/import_spur_app.rs` |
| `notebook_app_doctor`       | EXISTS   | `crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs` |
| `notebook_app_init`         | PLANNED  | (task U4 — not yet registered) |
| `notebook_app_pack`         | PLANNED  | (task U4 — not yet registered) |
| `spur app init`             | PLANNED  | (CLI task U5 — not yet shipped) |
| `spur app pack`             | PLANNED  | (CLI task U5 — not yet shipped) |
| `notebook_insert_cell`      | EXISTS   | notebook MCP |
| `notebook_write_cell`       | EXISTS   | notebook MCP |
| `notebook_read_cell`        | EXISTS   | notebook MCP |
| `notebook_run_cell`         | EXISTS   | notebook MCP |
| `notebook_save`             | EXISTS   | notebook MCP |

Do NOT call planned tools. Use the "today's real paths" column from the
front-door table in §5 below.
</HARD-GATE>

---

## The five-step loop

### 1. Declare capabilities in `spur-app.json`

The manifest declares what the app needs; the host provisions it. Never read
an env var the manifest did not declare — the host will not inject it.

Four capabilities exist today:

| Key                    | What the host provisions when declared |
|------------------------|----------------------------------------|
| `ports`                | Injects `SPUR_PORTS_ROOT` at plugin spawn. Doctor verifies declared `read` ports exist as DAG sources. |
| `canvas_capture`       | Guarantees the full recorder loop: `data-capture` canvas → `jute-video-capture` postMessage → `push_capture_port` → port store (includes `duration_sec`). Requires `active_output_scripts`. |
| `active_output_scripts`| Shows a one-time per-app trust prompt; on grant, output iframes run with `allow-scripts allow-same-origin`. |
| `artifacts_dir`        | Injects `SPUR_ARTIFACTS_DIR` and creates the directory. |

Minimal manifest with all four capabilities declared:

```json
{
  "schema": "spur.app/v1",
  "name": "my-app",
  "entry_notebook": "app.ipynb",
  "open_mode": "app",
  "runtime": { "jute_min": "0.1.0", "features": ["frontend-cells"] },
  "capabilities": {
    "ports": { "read": ["my-port"], "write": [] },
    "canvas_capture": true,
    "active_output_scripts": true,
    "artifacts_dir": true
  },
  "skill": "skill/SKILL.md"
}
```

Unknown capability keys are **rejected** at deserialization time (the Rust struct
uses `deny_unknown_fields`). Keep the keys exactly as above.

See `sdk/docs/manifest.md` for the full field reference and
`sdk/schema/spur-app.schema.json` for the JSON Schema.

### 2. Scaffold the app

**Today's real path (agents):** create files manually — see
`app_gallery/html_video/` as the reference app:
- `spur-app.json` — manifest (from §1 above)
- `app.ipynb` — the entry notebook
- `server/main.py` — the Python MCP server entry point
- `server/requirements.txt` — dependencies
- `skill/SKILL.md` — the app-specific agent skill

**Planned front doors (not yet available):**
- `notebook_app_init` (MCP, planned U4) — scaffolds the full structure from a
  template, doctor-green out of the box.
- `spur app init` (CLI, planned U5) — standalone equivalent.

### 3. Write server tools and frontend cells

**Python MCP server** — install `spur_app` from `sdk/python/` and use
`App` + capability properties:

```python
from spur_app import App

app = App("my-app")          # Name must match spur-app.json "name"

@app.tool()
def process(port_names: list[str], output_path: str) -> dict:
    # Read from the port store (requires capabilities.ports declared)
    frame = app.ports.read(port_names[0])
    # frame.bytes       — raw bytes
    # frame.mime        — MIME type or None (None for arrow ports)
    # frame.version     — version counter
    # frame.kind        — "arrow" or "media"
    # frame.duration_sec — seconds for media ports, or None
    # frame.path        — resolved filesystem Path that was read

    # Write to the artifacts dir (requires capabilities.artifacts_dir declared)
    out = app.artifacts.path(output_path)   # Path; parent dirs created
    out.write_bytes(frame.bytes)

    # Typed env-var accessors for custom env declared in mcp_server.env
    template_dir = app.env.path("TEMPLATES_DIR")
    return {"output": str(out)}

if __name__ == "__main__":
    app.run()          # stdio transport
```

If `SPUR_PORTS_ROOT` is not set (capabilities.ports not declared or host did
not inject), `app.ports` raises `MissingCapabilityError` on first access.
Same pattern for `app.artifacts` / `SPUR_ARTIFACTS_DIR`.

**TypeScript frontend cell** — each example below is a separate notebook cell.
`display.*` must be the last expression (or `return`) — Deno-Jupyter renders only
the cell return value.

Call a server tool and render its output (one cell):

```ts
import { callTool, display } from "@spur/app";

const result = await callTool("process", {
  port_names: ["my-port"],
  output_path: "renders/out.mp4",
});
return display.html(`<video controls src="..." />`);
```

Emit a capture canvas — requires `canvas_capture` + `active_output_scripts`
(one cell):

```ts
import { capture, display } from "@spur/app";

const html = capture.canvas({
  port: "my-cell-id",   // must match the DAG source cell id
  fps: 30,
  durationSec: 60,
  width: 1280,
  height: 720,
});
return display.html(html);
```

Read a port from the frontend (one cell):

```ts
import { ports } from "@spur/app";

const data = await ports.read("my-port");
// data.bytes, data.mime, data.version, data.kind, data.durationSec
```

`callTool` connects to `SPUR_NOTEBOOK_MCP_SOCKET` (injected by the host),
performs the full MCP initialize handshake, and calls `tools/call`.
See `sdk/docs/typescript-sdk.md` for the full API.

### 4. Run doctor before commit

`notebook_app_doctor` (exists) verifies the app from its root path:

```json
{ "path": "/abs/path/to/my-app" }
```

Returns `{ "ok": bool, "findings": [{check, level, message, location?}] }`.

Checks include: manifest parses; capabilities are known and grantable; declared
`ports.read` names exist as DAG sources; plugin spawns and `tools/list` succeeds;
skill file exists and every HARD-GATE tool name is in the live surface; port store
reachable; `runtime.features` warns if deprecated.

**Doctor must be green before packing.** A red doctor on any `fail`-level finding
should block the commit.

### 5. Pack and publish

**Today's real paths** — no planned tools needed:

| Step | Today (agents) | Planned (U4/U5) |
|------|----------------|-----------------|
| Pack to `.spurapp` | `notebook_export_spur_app { notebook_path, output_path }` | `notebook_app_pack` / `spur app pack` |
| Install / test pack | `notebook_import_spur_app { path }` | same tool |
| Scaffold | Manual (see §2) | `notebook_app_init` / `spur app init` |

`notebook_export_spur_app` creates the canonical `.spurapp` zip archive (Rust
packer in `spur_app::archive`). Never simulate the packer yourself — the fixture-
lockstep invariant guarantees that only the canonical packer produces a valid
archive.

```json
{
  "notebook_path": "/abs/path/to/my-app/app.ipynb",
  "output_path": "/abs/path/to/my-app.spurapp"
}
```

Optional parameters: `name` (override app name), `widget_assets` (list of widget
JS/CSS paths), `include_port_snapshots` (bool). The packer discovers dependency
lock files from the notebook's own parent directory — there is no `dependency_roots`
parameter.

---

## Testing

Use `spur_app.testing.FakePortStore` to test server tools without a live host:

```python
from spur_app.testing import FakePortStore
from pathlib import Path

FIXTURES = Path(__file__).parents[2] / "sdk" / "fixtures" / "port-store"

def test_process():
    with FakePortStore.from_fixtures(FIXTURES) as store:
        # SPUR_PORTS_ROOT is patched to the temp dir
        result = process(["spur-ad-capture"], "out.mp4")
        assert result["output"].endswith("out.mp4")
```

Or build a store programmatically:

```python
with FakePortStore() as store:
    store.add_media("clip", b"fake-bytes", mime="video/mp4", duration_sec=5.0)
    store.add_arrow("data", b"ipc-bytes")
    result = store.port_store.read("clip")
    assert result.duration_sec == 5.0
```

---

## References

- `sdk/docs/manifest.md` — `spur-app.json` field reference and capabilities
- `sdk/docs/python-sdk.md` — full Python SDK API
- `sdk/docs/typescript-sdk.md` — full TypeScript SDK API
- `sdk/docs/port-store.md` — port-store wire contract and fixtures
- `sdk/docs/dev-loop.md` — init/dev/doctor/pack/publish real paths vs planned
- `sdk/docs/versioning.md` — schema versioning, sdk_min, contract_version
- `sdk/schema/spur-app.schema.json` — JSON Schema for `spur-app.json`
- `app_gallery/html_video/` — reference app (first consumer of this SDK)

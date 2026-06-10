# Dev Loop — init / dev / doctor / pack / publish

This page documents the Spur app development loop. Some tools are already
available; others are planned for upcoming tasks. The table in each section
shows the real path to use today vs the planned front door.

## Status summary

| Step | Today (agents) | Planned | Task |
|------|----------------|---------|------|
| Init / scaffold | Manual (see below) | `notebook_app_init`, `spur app init` | U4/U5 |
| Dev (hot-reload) | N/A | `spur app dev` | U5 |
| Doctor | `notebook_app_doctor` (EXISTS) | same | — |
| Pack to .spurapp | `notebook_export_spur_app` (EXISTS) | `notebook_app_pack`, `spur app pack` | U4/U5 |
| Install / test | `notebook_import_spur_app` (EXISTS) | same | — |
| Publish | `notebook_export_spur_app` + manual | `spur app publish` | U5 |

## 1. Init / scaffold

### Today: manual

The reference app `app_gallery/html_video/` is the template. Copy and adapt:

```text
my-app/
  spur-app.json         # manifest (see docs/manifest.md)
  app.ipynb             # entry notebook
  server/
    main.py             # Python MCP server using spur_app
    requirements.txt    # at minimum: spur_app, mcp
  skill/
    SKILL.md            # app-specific agent skill
```

Minimal `server/main.py`:

```python
from spur_app import App

app = App("my-app")   # name must match spur-app.json

@app.tool()
def my_tool(x: str) -> str:
    return x.upper()

if __name__ == "__main__":
    app.run()
```

Minimal `spur-app.json` (no capabilities):

```json
{
  "schema": "spur.app/v1",
  "name": "my-app",
  "entry_notebook": "app.ipynb",
  "open_mode": "app",
  "runtime": { "jute_min": "0.1.0" },
  "mcp_server": { "type": "python", "entry": "server/main.py",
                  "requirements": "server/requirements.txt", "env": {} }
}
```

### Planned: notebook_app_init (U4)

```json
{ "app_root": "/abs/path/to/new-app", "name": "my-app", "template": "minimal" }
```

Will scaffold a doctor-green app structure from a template. Not yet registered.

### Planned: spur app init (U5 CLI)

```sh
spur app init my-app --template minimal
```

Talks to a running daemon when available; otherwise invokes the same Rust core
directly. Not yet shipped.

## 2. Dev (hot-reload)

### Today

No hot-reload tooling. To iterate:
1. Edit server files.
2. In the notebook daemon: close and reopen the app in app mode.

### Planned: spur app dev (U5 CLI)

Opens the app against a live daemon with plugin hot-restart on server-file change.

## 3. Doctor

`notebook_app_doctor` exists today. Pass the app root path or entry notebook path:

```json
{ "path": "/abs/path/to/my-app" }
```

Returns `{ "ok": bool, "findings": [...] }`:

```json
{
  "ok": false,
  "findings": [
    { "check": "manifest", "level": "pass", "message": "...", "location": "spur-app.json" },
    { "check": "skill:tool:phantom_tool", "level": "fail", "message": "HARD-GATE tool \"phantom_tool\" is referenced in skill but absent from both the notebook MCP tool registry and the plugin's tools/list", "location": "skill/SKILL.md" }
  ]
}
```

`ok` is `true` only when no finding has `level: "fail"`. Levels: `"pass"` | `"warn"` | `"fail"`.

**Run doctor before every pack.** A `fail`-level finding must be resolved before
packing. The doctor gate is the primary mechanism that prevents skills drifting
to phantom tools.

### What doctor checks (v1)

1. Manifest parses and `schema` is `"spur.app/v1"`; `entry_notebook` exists.
2. Every declared capability is known and grantable on this host.
3. Declared `capabilities.ports.read` names exist as DAG sources in the entry notebook.
4. Plugin spawns and `tools/list` succeeds.
5. Skill file at `manifest.skill` path exists; every tool name in its HARD-GATE block is in the live tool surface (plugin tools + notebook MCP tools).
6. Port store reachable at `SPUR_PORTS_ROOT`; fixtures version compatible.
7. `runtime.features` present → warn deprecated.

## 4. Pack

### Today: notebook_export_spur_app

```json
{
  "notebook_path": "/abs/path/to/my-app/app.ipynb",
  "output_path": "/abs/path/to/my-app.spurapp",
  "name": "my-app",
  "widget_assets": [],
  "include_port_snapshots": false
}
```

Only `notebook_path` and `output_path` are required. Optional: `name` (app name
override), `widget_assets` (list of widget JS/CSS paths), `include_port_snapshots`
(bool; bundles the current port store snapshot). There is no `dependency_roots`
parameter — the packer discovers lock files from the notebook's parent directory.

The packer:
- Bundles `app.ipynb` as `app.ipynb` in the archive.
- Collects widget assets (JS/CSS) into `widgets/<hash>.<ext>`.
- Discovers and bundles dependency lock files (`uv.lock`, `requirements.txt`,
  `deno.lock`, etc.) from the notebook's parent directory into `env/`.
- Generates `spur-app.json` from the notebook's metadata.
- Writes a zip archive with the `.spurapp` extension.

**Never simulate the packer yourself.** The fixture-lockstep invariant
(`INV-SDK-F1`) ensures only the canonical Rust packer (`spur_app::archive`)
produces a valid archive. App tests that hand-roll a `.spurapp` are wrong.

### Planned: notebook_app_pack (U4) / spur app pack (U5)

Will wrap `notebook_export_spur_app` with pre-pack doctor gate and richer UX.

## 5. Install / test the pack

```json
{ "path": "/abs/path/to/my-app.spurapp" }
```

`notebook_import_spur_app` extracts the bundle into a content-addressed cache
(`~/.spur/apps/sha256-<hash>/`) and returns the extracted root and notebook path.
The extracted app can then be opened in app mode.

## 6. Publish

### Today

1. Run doctor: `notebook_app_doctor { "path": "..." }` — must be green (`ok: true`).
2. Pack: `notebook_export_spur_app` as above.
3. Distribute the `.spurapp` file manually.

### Planned: spur app publish (U5)

Pack + checksums + doctor gate in one command. Will integrate with a future app
registry (not in scope for v1).

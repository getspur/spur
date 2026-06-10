# spur-app.json — Manifest Reference

`spur-app.json` is the manifest for a Spur notebook app bundle (`.spurapp`).
The host reads it at open time to configure the plugin, grant capabilities, and
inject environment variables.

Source of truth for types: `crates/spur-notebook/src/spur_app.rs`
JSON Schema: `sdk/schema/spur-app.schema.json`

## Top-level fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `schema` | `"spur.app/v1"` | YES | — | Must be the literal string `"spur.app/v1"`. |
| `name` | string | YES | — | App name; used as the MCP server name. Must match `App("name")` in the Python server. |
| `entry_notebook` | string | YES | — | Relative path inside the bundle to the entry `.ipynb` notebook (e.g. `"app.ipynb"`). |
| `open_mode` | `"app"` \| `"notebook"` | YES | — | `"app"` opens in full app mode (trust-granted output iframes, plugin spawn). `"notebook"` opens as a plain notebook. |
| `runtime` | object | YES | — | See [Runtime](#runtime). |
| `widgets` | array | no | `[]` | Bundled widget JS/CSS assets. Usually populated by the packer. |
| `ports` | object \| null | no | `null` | Port snapshot options. `null` means no snapshots. |
| `dependencies` | object | no | `{}` | Paths to bundled lock files. Populated by the packer from dependency roots. |
| `mcp_server` | object \| null | no | `null` | MCP server configuration. `null` means the app has no server plugin. |
| `capabilities` | object | no | all off | Capability declarations. See [Capabilities](#capabilities). |
| `skill` | string \| null | no | `null` | Relative path to the app's agent skill file. Defaults to `"skill/SKILL.md"`. |

## Runtime

```json
"runtime": {
  "jute_min": "0.1.0",
  "features": ["frontend-cells", "anywidget-afm", "ports-arrow"]
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `jute_min` | string | YES | Minimum host semver required. |
| `features` | array of string | no | Deprecated feature flags. Honored but never enforced; doctor warns if present. Use `capabilities` instead. |

## Capabilities

The `capabilities` object is the **only** way to get host-provisioned env vars.
Never read an env var the manifest did not declare — the host will not inject it.

```json
"capabilities": {
  "ports": { "read": ["spur-ad-capture"], "write": [] },
  "canvas_capture": true,
  "active_output_scripts": true,
  "artifacts_dir": true
}
```

**Unknown keys are rejected** at deserialization time (`deny_unknown_fields` in
Rust). The host returns a structured error naming the unknown key and the manifest
path. Keep keys exactly as documented below.

| Key | Type | Default | Host semantics when declared |
|-----|------|---------|------------------------------|
| `ports` | `{read: string[], write: string[]}` \| null | null | Injects `SPUR_PORTS_ROOT` at plugin spawn. Doctor verifies declared `read` names exist as DAG sources in the entry notebook. |
| `canvas_capture` | boolean | false | Guarantees the canvas-capture recorder loop end-to-end: `data-capture` canvas → `jute-video-capture` postMessage → `push_capture_port` → port store (including `duration_sec`). Requires `active_output_scripts: true`. |
| `active_output_scripts` | boolean | false | Shows a one-time per-app trust prompt. On grant, output iframes run with `allow-scripts allow-same-origin`. Without this, canvas capture silently never fires. |
| `artifacts_dir` | boolean | false | Injects `SPUR_ARTIFACTS_DIR` at plugin spawn and creates the directory. |

## MCP server

```json
"mcp_server": {
  "type": "python",
  "entry": "server/main.py",
  "requirements": "server/requirements.txt",
  "env": {
    "TEMPLATES_DIR": "server/templates"
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `"python"` | YES | Server runtime. Only `"python"` is supported in v1. |
| `entry` | string | YES | Archive-relative path to the server entry point. |
| `requirements` | string \| null | no | Archive-relative path to the Python requirements file. |
| `env` | object (string values) | no | Static env vars injected at spawn. Host-provisioned vars (`SPUR_PORTS_ROOT`, `SPUR_ARTIFACTS_DIR`) are merged after these; host wins on conflict. |

## Additive-compatibility rule

Existing manifests without `capabilities` or `skill` deserialize unchanged with
all capabilities defaulted to off. The manifest root uses permissive
`additionalProperties` (unknown root-level keys are ignored); only the inner
`capabilities` object is strict (`additionalProperties: false`).

## Example

See `app_gallery/html_video/spur-app.json` for the reference app manifest.

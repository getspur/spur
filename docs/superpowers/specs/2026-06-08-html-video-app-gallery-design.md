# html_video Notebook Mini-App — Design Spec

**Design epic:** `bd-1wzn`
**Date:** 2026-06-08

**Goal:** Extract html_video from the spur-notebook crate into a self-contained, deployable notebook mini-app in `app_gallery/html_video/`, powered by a foundation-level generic MCP plugin system.

---

## 1. Architectural Boundary

**Foundation (stays in `spur-notebook` crate — generic, no app-specific code):**
- `PortStore` and `PortRead::Media` variant (dag/ports.rs)
- DAG engine, reactive cascade, kernel management
- MCP server framework and tool registration infrastructure
- `.spurapp` packaging system (spur_app.rs)
- **New:** Generic MCP plugin loader (spawns app MCP servers, proxies tool calls)

**App (`app_gallery/html_video/` — self-contained, deployable):**
- Python MCP server (html_video_render, html_video_search_templates, html_video_get_template)
- Template content (HTML files, index.json)
- Entry notebook (app.ipynb with DAG pipeline + frontend cells)
- Brain skill (workflow definition)
- App tests

**Key invariant:** The foundation knows nothing about html_video. It only knows: "this SpurApp declares an MCP server of type `python` — spawn it and proxy tool calls."

## 2. Foundation Plugin Loader

The foundation gains one generic new capability: MCP server plugin loading.

### Behavior

1. On import of a `.spurapp`, reads `mcp_server` field from `spur-app.json`
2. On app open (App mode), spawns the MCP server as a child process using stdio JSON-RPC transport
3. Queries child for tool list via MCP `tools/list`
4. Merges app tools into the main tool registry
5. On `tools/call`, routes by tool name to the child process
6. Manages lifecycle: start on app open, stop on app close, health check via MCP `ping`

### Manifest Schema Addition

```json
{
  "schema": "spur.app/v1",
  "name": "html-video",
  "entry_notebook": "app.ipynb",
  "open_mode": "app",
  "mcp_server": {
    "type": "python",
    "entry": "server/main.py",
    "requirements": "server/requirements.txt",
    "env": {
      "TEMPLATES_DIR": "templates",
      "SPUR_PORTS_ROOT": "$resolved_by_foundation"
    }
  },
  "runtime": { "jute_min": "0.1.0", "features": ["frontend-cells", "anywidget-afm", "ports-arrow"] },
  "widgets": [],
  "ports": null
}
```

### Foundation Rust Changes

- `SpurAppManifest` gains `mcp_server: Option<SpurAppMcpServer>`
- New `SpurAppMcpServer` struct: `{ server_type, entry, requirements, env }`
- New `mcp/plugin_loader.rs` — spawns child processes, proxies MCP calls, manages lifecycle
- `mcp/tools/mod.rs` — merges app plugin tools into the main registry
- `ServerDeps` gains `plugins: Option<Arc<PluginRegistry>>` (see ServerDeps Integration below)

**Env var injection:** The `env` field in `spur-app.json` declares app-specific env vars. The foundation additionally injects `SPUR_PORTS_ROOT` (resolved from `notebook_port_root()`) at spawn time, regardless of manifest contents. The `$resolved_by_foundation` placeholder in the example above is illustrative — the manifest author does not set this value.

### ServerDeps Integration

The plugin loader must integrate with `ServerDeps` (blast radius 9.69 in spur-notebook — the central dependency injection struct). This is an internal foundation change, not a public API break.

- `ServerDeps` gains `pub plugins: Option<Arc<PluginRegistry>>` — holds spawned plugin process handles
- `PluginRegistry` owns a map of `{plugin_name: ChildProcess}` and manages spawn/stop lifecycle
- The existing `bridge`, `state`, `app`, `daemon` fields are unchanged
- Construction sites (`ServerDeps::new`, test helpers) pass `None` for plugins by default

### Tool Routing Mechanism

The foundation routes MCP tool calls to plugin processes using a name-based routing map.

1. **Discovery:** On app open, after spawning the plugin, foundation calls MCP `tools/list` on the child process
2. **Registration:** Foundation builds a routing map: `{tool_name: plugin_process_handle}` from the discovered tools
3. **Dispatch:** On `tools/call`, the MCP server checks the routing map first. If the tool name matches a plugin tool, the request is forwarded to the child process via stdio JSON-RPC. If not, it falls through to the existing static `tools()` registry.
4. **Name collision:** If a plugin tool name collides with a foundation tool, the foundation tool wins (plugin tools are additive, not overriding). Foundation logs a warning on collision.

This means plugin tools are dynamically discovered, not statically known. The routing map is rebuilt when an app is opened or re-imported.

### What the Foundation Does NOT Do

- Does not inspect tool names (beyond collision detection)
- Does not validate tool schemas
- Does not cache tool results
- Does not know about ffmpeg, templates, or video

### Backward Compatibility

- Existing `.spurapp` packages without `mcp_server` work unchanged
- Existing Rust MCP tools continue to work alongside app plugin tools
- No breaking changes to any foundation API

## 3. App Directory Structure

```
app_gallery/html_video/
├── spur-app.json              # App manifest
├── app.ipynb                  # Entry notebook (DAG + frontend cells)
├── README.md                  # App documentation
│
├── server/                    # Python MCP server
│   ├── main.py               # Entry point: stdio MCP server
│   ├── requirements.txt      # Python deps: mcp, ffmpeg-python, Pillow
│   ├── render.py             # ffmpeg pipeline (webm→mp4)
│   ├── library.py            # Template search/score/load
│   └── tools/
│       ├── render.py         # html_video_render tool handler
│       ├── search.py         # html_video_search_templates handler
│       └── get_template.py   # html_video_get_template handler
│
├── templates/                 # Template content
│   ├── index.json            # Template manifest
│   ├── frame-glitch-title/
│   │   ├── template.html
│   │   └── SKILL.md
│   ├── frame-data-rollup/
│   │   ├── template.html
│   │   └── SKILL.md
│   └── frame-liquid-hero/
│       ├── template.html
│       └── SKILL.md
│
├── skill/                     # Brain workflow
│   ├── SKILL.md              # 6-step loop
│   └── references/
│       └── video-mode.md     # Content-graph IR spec
│
└── tests/                     # App-level tests
    ├── test_render.py
    ├── test_library.py
    └── fixtures/
```

### What Gets Removed from the Crate

- `crates/spur-notebook/assets/html-video-library/` (moved to `templates/`)
- `crates/spur-notebook/src/html_video/` module (logic moved to Python server)
- `crates/spur-notebook/src/mcp/tools/html_video_*.rs` (moved to Python server)
- `.spur/skills/html-video/` content (moved to `skill/`, thin pointer left behind)

### What Stays in the Crate

- `dag/ports.rs` — PortStore media support
- DAG engine, reactive cascade
- MCP server framework
- `.spurapp` packaging system
- The new generic plugin loader

## 4. Python MCP Server Design

### Entry Point

```python
from mcp.server import Server
from mcp.server.stdio import stdio_server

server = Server("html-video")

from tools import render, search, get_template
render.register(server)
search.register(server)
get_template.register(server)

async def main():
    async with stdio_server() as (read, write):
        await server.run(read, write)

if __name__ == "__main__":
    import asyncio
    asyncio.run(main())
```

### Tool Contract

| Tool | Input | Output |
|---|---|---|
| `html_video_render` | `{webm_frames?, port_names?, output_path, resolution?, fps?, frame_duration?}` | `{output_path, frame_count, fps, duration, resolution}` |
| `html_video_search_templates` | `{intent, top?}` | `{items: [{id, title, intent, summary, tags, score}]}` |
| `html_video_get_template` | `{id}` | `{id, metadata, html, skill_md}` |

### Port Access from Python

The actual PortStore on-disk layout is `~/.spur/notebooks/nb-<blake3_hash>/ports/<port_name>`, NOT adjacent to the notebook directory. The path derivation (`notebook_port_root()`) uses blake3 hashing of the full notebook path and lives in the Tauri crate (`jute-notebook/src-tauri/src/ports.rs`).

**The Python server cannot derive this path.** The foundation resolves it at spawn time and passes it as the `SPUR_PORTS_ROOT` environment variable.

- Foundation sets `SPUR_PORTS_ROOT=<resolved port root>` when spawning the plugin process
- Python server reads `os.environ["SPUR_PORTS_ROOT"]` to locate media blobs
- Media files are at `$SPUR_PORTS_ROOT/<port_name>` (one file per port, versioned)
- No Rust FFI needed — the on-disk layout (directory structure, manifest.json, file naming) is a documented, stable contract

### Template Resolution

- Reads `TEMPLATES_DIR` env var (set by foundation from manifest `env` field)
- Falls back to `./templates` relative to server entry point
- `index.json` + per-template directories (same schema as current Rust implementation)

### ffmpeg Interaction

- `subprocess.run(["ffmpeg", ...])` for encoding
- `shutil.which("ffmpeg")` for availability check
- Temp directory for scratch files, cleaned up after render

## 5. Data Flow

```
notebook cell (Python/Deno)
    │ calls html_video_render via notebook MCP
    ▼
Foundation MCP Server (Rust, spur-notebook)
    │ recognizes tool belongs to app plugin
    │ routes to child process via stdio JSON-RPC
    ▼
Python MCP Server (child process, app-owned)
    │ 1. Reads webm frames from $SPUR_PORTS_ROOT/<port_name>
    │    (or decodes base64 webm_frames param)
    │ 2. Writes frames to temp dir
    │ 3. Calls ffmpeg via subprocess
    │ 4. Returns output_path + metadata
    ▼
Foundation MCP Server
    │ proxies result back to notebook
    ▼
notebook cell receives result
    │ creates <video> tag with mp4 output
    ▼
App mode renders video in frontend cell
```

## 6. Task Decomposition Boundaries

### Phase 1: Foundation Plugin System (generic)
- **Task A:** Manifest schema extension (`SpurAppMcpServer` struct, serde)
- **Task B:** Plugin loader runtime (spawn, stdio, proxy, lifecycle)
- **Task C:** Tool routing integration — defines the routing *mechanism* (routing map, `tools/list` query, dispatch logic). Does NOT require knowing specific tool names. Any plugin providing MCP tools is routable.
- **Task D:** Plugin loader tests including a **hello-world smoke test**: a trivial Python server exposing one tool (`echo`), verifying spawn → `tools/list` → `tools/call` → stop lifecycle end-to-end

### Phase 2: App Extraction (html_video specific)
- **Task E:** Create `app_gallery/html_video/` structure
- **Task F:** Move template content from crate assets
- **Task G:** Move skill from `.spur/skills/html-video/`
- **Task H:** Build Python MCP server (render, search, get_template)
- **Task I:** Create `spur-app.json` manifest
- **Task J:** Remove html_video Rust code from crate

### Phase 3: Integration
- **Task K:** Create `app.ipynb` entry notebook
- **Task L:** End-to-end test (import .spurapp, open app, render video)
- **Task M:** Export as `.spurapp` package, verify import cycle

### Parallelism
- Tasks A/B/C/D (foundation) can run in parallel with Tasks E/F/G (app structure)
- Task C defines the routing *interface* (mechanism only, no specific tool names). Task H provides the concrete html_video tools. No circular dependency — C is testable with the hello-world plugin from Task D.
- Task H (Python server) depends on F (templates in place) and Task C (routing mechanism exists)
- Task J (remove Rust code) depends on H (Python server working) and Task D (hello-world proves routing)
- Tasks K/L/M depend on J

## 7. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Plugin loader introduces latency on tool calls | stdio JSON-RPC is fast (~1-5ms round-trip); measure before optimizing |
| Python server crashes take down tool availability | Foundation monitors child process health, restarts on crash. In-flight requests return error. |
| PortStore filesystem access from Python requires path derivation | Foundation resolves path and passes `SPUR_PORTS_ROOT` env var at spawn time. Python reads env, never derives path. |
| ServerDeps extension has high blast radius (9.69) | `plugins` field is `Option<Arc<PluginRegistry>>` with `#[serde(default)]` semantics. Existing construction sites pass `None`. |
| Concurrent tool calls serialize on stdio | Single-threaded stdio assumption documented. Plugin processes handle one request at a time. Foundation queues. |
| Template resolution breaks on move | App declares `TEMPLATES_DIR` env var; foundation sets it at spawn time |
| Python port diverges from Rust render implementation (447 lines, 15 functions) | Port function-by-function. Existing Rust tests serve as behavioral specification. |
| Existing html_video tests break | Migrate tests to Python test suite; keep integration test in Rust via plugin loader |

## 8. Security and Trust Model

Installing a `.spurapp` is equivalent to running its code. The plugin loader spawns an arbitrary subprocess declared in `spur-app.json` — this is by design, not a vulnerability.

**Trust model:**
- Users must trust app authors before importing a `.spurapp`
- The `spur-app.json` manifest is reviewed during import (same as reviewing any code dependency)
- No sandboxing is provided in Phase 1

**Defense-in-depth (future):**
- Plugin runs in app directory only (chroot or namespace isolation)
- Network access disabled by default (opt-in via manifest flag)
- Resource limits (CPU/memory caps via cgroups or rlimits)

**Current scope:** The foundation spawns the exact entry point declared in the manifest. It does not execute arbitrary code beyond what the manifest specifies. Security is the user's responsibility, same as `pip install` or `npm install`.

## 9. Out of Scope

- `notebook_get_cell_capture` MCP tool (separate design — canvas capture infrastructure)
- Canvas capture in jute-notebook frontend (separate design)
- App mode UI for interactive video editing (future phase)
- Multi-app plugin composition (future design)
- Plugin sandboxing, network restrictions, or resource limits (future defense-in-depth)

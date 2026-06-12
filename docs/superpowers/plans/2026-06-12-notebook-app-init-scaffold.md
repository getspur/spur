# notebook_app_init Scaffolding Tool Implementation Plan (U4 init front door)

**Spec:** `docs/superpowers/specs/2026-06-10-spur-app-sdk-design.ipynb` §5 (dev-loop tooling), §8 (U4).
**Goal:** Replace the manual "copy `app_gallery/html_video/` and adapt" init step with a
`notebook_app_init` MCP tool that scaffolds a doctor-green Spur App from a named template,
so a developer (or agent) gets from zero to a runnable polyglot app in one call.

## Problem

`sdk/docs/dev-loop.md` §1 documents init as **manual**: copy the reference app and adapt
seven files by hand. That is the single worst step of the DX journey (the spec's success
metric is "a second gallery app costs ~1/10 of the first"). Export (`notebook_export_spur_app`),
import, and doctor (`notebook_app_doctor`) already exist as notebook MCP tools; init does not.

## Design

### Layer 1 — `spur_app::scaffold` (Rust core, reusable by the future U5 CLI)

New module `crates/spur-notebook/src/spur_app/scaffold.rs`:

- `ScaffoldOptions { app_root, name, template }` → `scaffold_app()` → `ScaffoldedApp { app_root, files }`.
- `ScaffoldError` (thiserror): `InvalidName`, `UnknownTemplate`, `AppRootNotEmpty`, `Io`.
- **Template registry** — `templates()` returns `&'static [TemplateInfo { name, description }]`.
  Adding a new app model type (a future `deno` server template, a `dashboard` template, …)
  is one new registry entry plus its file table; no tool/dispatch changes. v1 templates:
  - `minimal` — Python MCP server on the `spur_app` SDK + TypeScript frontend cell on the
    vendored TS SDK + skill + pytest tests via `spur_app.testing`. The polyglot reference shape.
  - `frontend-only` — no `mcp_server`; markdown + HTML-output frontend cell. Demonstrates a
    second app model type and gives a fast doctor path (no plugin spawn).
- Generated files (minimal): `spur-app.json` (serialized from a real `SpurAppManifest`, so it
  can never drift from the model; `runtime.features` left empty — they are deprecated),
  `app.ipynb` (nbformat 4.5, cell ids, `metadata.spur` like the gallery apps), `server/main.py`,
  `server/requirements.txt`, `skill/SKILL.md` (HARD-GATE referencing only live tool names),
  `conftest.py`, `tests/test_app.py`, `sdk/call_tool.ts`, `sdk/wire.ts`.
- **Vendored TS SDK lockstep:** `sdk/call_tool.ts` + `sdk/wire.ts` are embedded with
  `include_str!` from `sdk/typescript/src/`, so scaffolded copies are byte-identical to the
  canonical SDK at the compiled commit — same invariant the html_video drift-guard test pins.
- **Python SDK requirement resolution:** the plugin loader resolves requirement paths against
  the app root (cwd). Walk up from `app_root` looking for `sdk/python/pyproject.toml` (the
  monorepo case) and emit that relative path, mirroring `app_gallery/html_video`; otherwise
  emit `mcp>=1.0.0` plus a `TODO(U7)` comment pointing at the PyPI release.

### Layer 2 — `notebook_app_init` MCP tool

`crates/spur-notebook/src/mcp/tools/notebook_app_init.rs`:

- Params: `{ app_root: string, name?: string (default: app_root dir name), template?: string (default "minimal") }`.
- Path rules follow `validate_notebook_path` conventions (no `..`, relative resolved to cwd).
- Result: `{ ok, app_root, template, files[], next_steps[] }` — next_steps walk the dev loop:
  doctor → open in app mode → pack.
- Registered in `tools/mod.rs::tools()` and dispatched in `mcp/mod.rs` next to
  `notebook_app_doctor`.

### Docs

`sdk/docs/dev-loop.md` §1 + status table flip init from "Manual / planned U4" to
`notebook_app_init` (EXISTS); `spur app init` stays planned (U5). `sdk/skill/SKILL.md` and
`sdk/llms.txt` updated where they describe init as manual.

## Acceptance (from spec §8)

- Scaffolded `frontend-only` app passes `notebook_app_doctor` with `ok: true` in a Rust test.
- Scaffolded `minimal` app: manifest round-trips through `SpurAppManifest`, vendored SDK files
  byte-match `sdk/typescript/src/`, notebook parses as nbformat 4, server/tests reference only
  `spur_app` APIs (zero hand-written protocol code).
- `tools()` exposes `notebook_app_init`; unknown template / occupied app_root / bad name are
  structured `invalid_params` errors.

## Tasks

1. `test(spur-notebook)` failing tests for `spur_app::scaffold` (registry, file table, manifest
   round-trip, vendored lockstep, error cases).
2. `feat(spur-notebook)` implement scaffold module.
3. `test+feat(spur-notebook)` `notebook_app_init` tool, registration, dispatch, doctor-green test.
4. `docs(sdk)` dev-loop/skill/llms.txt updates.

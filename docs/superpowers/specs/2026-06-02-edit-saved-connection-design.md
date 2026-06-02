# Edit Saved Connection — Design

**Date:** 2026-06-02
**Status:** Approved (brainstorming complete)
**Surface:** `crates/spur-notebook/jute-notebook` datasource wizard + `crates/spur-notebook` daemon

## Problem

Saved API connections (`ConnectionTemplate`, stored in `~/.spur/connections.json`) can
today only be **attached** or **deleted** from the Datasource sidebar. There is no way to
edit an existing saved connection. A user who wants to tweak a connection's configuration
must delete it and re-add it from scratch via the Add REST API wizard.

We want to support editing an existing saved connection by **reloading its configuration
into the wizard**, so the user follows the same journey they already know — only the fields
start pre-populated.

## Goals

- Edit a saved connection through `AddRestApiWizard`, reusing the existing
  Connect → Tables → Review journey unchanged.
- Pre-populate every field from the selected `ConnectionTemplate`.
- Preserve the stored tables/manifest when the user does not supply a new spec.
- Persist edits back to the global connection store, keyed by the connection name.

## Non-Goals

- Renaming a connection (name is the store key; locked in edit mode — see Decisions).
- Persisting credential secret **values** (they are never stored; this does not change).
- Persisting the original OpenAPI `specText` (out of scope; see Accommodation).
- An inline (non-wizard) edit surface in the sidebar.

## Decisions (from brainstorming)

| # | Decision | Rationale |
|---|----------|-----------|
| Q2 | Edit opens the wizard **past the Source step**, straight to Connect. | Source ("how you connected") can't change after the fact; only config can. |
| Q3 | Connection **name is read-only** in edit mode. | `name` is the `upsert` key; locking it avoids orphaned templates and datasource/template desync. |
| Q4 | **Hybrid**: same journey, fields pre-filled, no restricted edit surface. | Matches the user's mental model — "same wizard, config already there." |

## Background — how saved connections work today

- A `ConnectionTemplate` is created automatically when `add_api_datasource_from_import`
  completes (`crates/spur-notebook/src/mcp/mod.rs:1712`). It is upserted by `name`
  (`connection_store::upsert`, `crates/spur-notebook/src/connection_store.rs:43`), which
  **overwrites by name and preserves `created_at`**.
- The template stores: `name`, `provider`, `group`, `manifest_toml` (the **generated**
  manifest incl. tables), `tables`, `credential_env_vars` (**keys only** — secret values
  are deliberately not persisted), `created_at`, `updated_at`.
- The original OpenAPI `spec_text` is **not** persisted — only the manifest it produced.
- Daemon commands today: `ListSavedConnections`, `AttachSavedConnection`,
  `DeleteSavedConnection` (`DaemonControlCommand` @ `commands.rs:269-368`). There is **no**
  update/edit command. Each handler has a real impl + a `#[cfg]` stub in `mcp/mod.rs`.

## The one accommodation

The only place the edit journey cannot be literally identical to a fresh add: the original
`specText` is not persisted, so the Tables step's spec textarea cannot be pre-filled. Behavior:

- **Spec left blank** → preserve the stored `manifest_toml` + `tables` verbatim.
- **New spec pasted** → regenerate `manifest_toml` + `tables` wholesale via the existing
  import path.

To make the Tables step satisfy its "Continue" gate without a spec, the wizard seeds
`tablePreview` from `connection.tables` in edit mode so the existing tables render and the
step is considered complete.

## Architecture

Reuse `AddRestApiWizard` end-to-end with an optional edit-mode input. No new component.

### Frontend

**`AddRestApiWizard.tsx`**
- New optional prop: `editConnection?: ConnectionTemplate | null`.
- When `editConnection` is set (edit mode):
  - On open, initialize state from the template instead of the empty defaults:
    - `datasourceName` ← `editConnection.name` (rendered **read-only**).
    - `sourceMode` ← a fixed edit sentinel (Source step skipped); `selectedProvider` stays `null`.
    - credential fields derived from `editConnection.credentialEnvVars` (not the provider
      heuristic), rendered empty, optional, labeled "session only / leave blank to keep".
    - `tablePreview` seeded from `editConnection.tables`; `specText` empty.
    - `stepIndex` starts at **Connect**; the Source step renders as completed + locked.
  - Submit routes to the new `updateSavedConnectionCommand` instead of
    `addApiDatasourceFromImportCommand`.
  - Primary action label on Review becomes **"Save changes"**.
- When `editConnection` is null/absent, behavior is exactly as today.

**`DatasourceSidebar.tsx`**
- Add an **Edit** button in the expanded `SavedConnectionRow` (beside "Delete saved
  connection"). Clicking it stores the selected `ConnectionTemplate` and opens the wizard
  in edit mode.
- Owns a new piece of state (e.g. `editingConnection: ConnectionTemplate | null`) passed to
  `AddRestApiWizard`; cleared on close. The existing `connections://changed` listener
  already refreshes the list after save.

**`control.ts`**
- New `UpdateSavedConnectionCommand` type + `updateSavedConnectionCommand(input)` builder.
- Input shape: `{ name: string; spec_text: string | null; credentials?: [string, string][] }`.
- Response decoded with the existing `attachedSavedConnectionFromDaemonControlResponse`
  (`{ entry, missingEnvVars }`) — the update handler returns the same
  `AttachedSavedConnection` payload as attach, so the wizard can report missing env vars on save.

### Backend (Rust)

**`commands.rs`** — new additive variant on `DaemonControlCommand`:

```rust
/// Update a saved connection template in place (edit flow).
UpdateSavedConnection {
    name: String,
    spec_text: Option<String>,
    #[serde(default)]
    #[ts(type = "[string, string][]")]
    credentials: Vec<(String, String)>,
},
```

**`mcp/mod.rs`** — new `update_saved_connection` handler (real impl behind
`datasource-introspect` + `#[cfg]` stub, matching the existing trio) and a match arm in the
dispatch (~`mod.rs:1355`):

- Inject provided credential values into the session env (`std::env::set_var`), same as
  import/attach. Values are not persisted.
- If `spec_text` is `Some`: regenerate manifest + tables via the existing
  `build_api_import_manifest` path, then `connection_store::upsert` (preserves `created_at`,
  refreshes `updated_at` + `credential_env_vars`).
- If `spec_text` is `None`: load the existing template by name; reuse its `manifest_toml` +
  `tables`; re-register the datasource entry; `connection_store::upsert` to refresh
  `updated_at` (and `credential_env_vars` if credential keys were supplied, else keep).
- Error `saved_connection_not_found` if the named template no longer exists (stale edit).
- Return the `AttachedSavedConnection` payload (`{ entry, missing_env_vars }`), same as the
  attach handler.
- Emit `connections://changed` on success.

ts-rs regen flows the new variant into the `DaemonControlCommand` TS union automatically.

## Data flow

```
Sidebar (expanded row) ──Edit──▶ DatasourceSidebar.editingConnection = template
        │
        ▼
AddRestApiWizard(editConnection=template)
  init: name(ro), credEnvVars→fields, tablePreview←template.tables, stepIndex=Connect
        │  (Connect → Tables → Review, all pre-filled)
        ▼
"Save changes" ──▶ updateSavedConnectionCommand({ name, spec_text, credentials })
        │
        ▼
daemon UpdateSavedConnection
  spec_text=None  → keep manifest+tables, re-register, upsert(updated_at)
  spec_text=Some  → regenerate manifest+tables, upsert(updated_at)
  → emit connections://changed
        │
        ▼
Sidebar list refreshes via existing listener
```

## Error handling

Unchanged patterns:
- Daemon errors surface in the wizard's `ErrorBanner`.
- `saved_connection_not_found` when the template was deleted between open and save.
- Missing credentials behave as today (the connection attaches; missing env vars are
  reported by the attach path / sidebar notice).

## Testing

- **`control.test.ts`** — `updateSavedConnectionCommand` shape (with/without credentials,
  null spec_text).
- **`commands.rs` tests** — `UpdateSavedConnection` (de)serialization round-trip.
- **`api_datasource_import_e2e.rs`** — edit round-trip:
  1. import a connection, then update with `spec_text=None` → manifest/tables preserved,
     `created_at` unchanged, `updated_at` advanced.
  2. update with a new `spec_text` → manifest/tables regenerated.
  3. update a non-existent name → `saved_connection_not_found`.

## Touch points (summary)

- `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` — enum variant.
- `crates/spur-notebook/src/mcp/mod.rs` — handler (×2) + match arm.
- ts-rs binding regen (`DaemonControlCommand` union).
- `crates/spur-notebook/jute-notebook/src/daemon/control.ts` — builder + Input type.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx` — edit mode.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/DatasourceSidebar.tsx` — Edit button.
- Tests: `control.test.ts`, `commands.rs`, `api_datasource_import_e2e.rs`.

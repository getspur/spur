# SpurApp Packaging and Delivery Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-06-spur-app-packaging-delivery-research.ipynb`
**Design epic:** `bd-3612`

**Goal:** Implement the first `.spurapp` packaging milestone: schema, archive read/write, import/export services, MCP tools, and runtime file-association support.

**Architecture:** SpurApp is a SPUR-branded application artifact; Jute remains the current Tauri runtime shell. The implementation adds a crate-local `spur_app` module in `spur-notebook` that owns `.spurapp` archive semantics and keeps `spur-app.json` as packaging metadata while `app.ipynb` remains the source-of-truth notebook. MCP and file-association entry points call this module rather than duplicating archive logic.

**Delivery Contract:** A user can develop a SpurApp locally from a notebook, export it as a `.spurapp` package, send that package to a teammate who already has Jute notebook installed, and the teammate can open the package as an app. The opened app must use the embedded `app.ipynb`, bundled widget assets, copied dependency lock metadata, and included port snapshots so it runs equivalently to the original on a compatible Jute/runtime environment. Import preflight must report missing local dependencies or unsupported optional state instead of silently producing a degraded app.

**Tech Stack:** Rust 2021, `serde`, `serde_json`, `blake3`, `zip`, existing Jute notebook model types, notebook MCP tools, Tauri file associations. Build and test through `scripts/spur-cargo`.

---

## File Structure Mapping

- `crates/spur-notebook/src/spur_app.rs`
  - New public module for constants, manifest structs, validation, export/import options, preflight result types, cache path helpers, and archive entry safety checks.
- `crates/spur-notebook/src/spur_app/archive.rs`
  - New archive implementation. Owns deterministic `.spurapp` zip read/write, content hashing, safe extraction, and round-trip tests.
- `crates/spur-notebook/src/lib.rs`
  - Exposes `pub mod spur_app;`.
- `crates/spur-notebook/Cargo.toml`
  - Adds `zip = "2"` to `spur-notebook` only. This dependency is justified because no archive crate currently exists in the workspace and `.spurapp` is a zip-like distributable artifact.
- `crates/spur-notebook/src/mcp/tools/export_spur_app.rs`
  - New MCP tool `notebook_export_spur_app`.
- `crates/spur-notebook/src/mcp/tools/import_spur_app.rs`
  - New MCP tool `notebook_import_spur_app`.
- `crates/spur-notebook/src/mcp/tools/mod.rs`
  - Registers the new tools and allows `.spurapp` paths for these methods only.
- `crates/spur-notebook/src/main.rs`
  - Resolves `.spurapp` launch arguments by importing into the local cache and opening the embedded notebook.
- `crates/spur-notebook/jute-notebook/src-tauri/tauri.conf.json`
  - Adds `.spurapp` to file associations.
- `crates/spur-notebook/tests/spur_app_archive.rs`
  - New integration tests for archive export/import and path safety.
- `crates/spur-notebook/tests/spur_app_mcp_tools.rs`
  - New integration tests for MCP tool payload validation and registered tool names.

## Dependency DAG

```text
task-1-core-schema-archive
  -> task-2-export-import-service
       -> task-3-mcp-tools
       -> task-4-file-association
            -> task-5-integration-docs
```

`task-3-mcp-tools` and `task-4-file-association` can run in parallel after `task-2-export-import-service`.

---

### Task 1: Core SpurApp Schema and Archive Primitives

**Task ID:** `task-1-core-schema-archive`

**Files:**
- Create: `crates/spur-notebook/src/spur_app.rs`
- Create: `crates/spur-notebook/src/spur_app/archive.rs`
- Create: `crates/spur-notebook/tests/spur_app_archive.rs`
- Modify: `crates/spur-notebook/src/lib.rs`
- Modify: `crates/spur-notebook/Cargo.toml`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `spur-notebook` exposes a `spur_app` module.
- [ ] `.spurapp` and `spur-app.json` constants exist and no legacy package naming is introduced.
- [ ] `SpurAppManifest` serializes and deserializes `schema: "spur.app/v1"`.
- [ ] Archive extraction rejects absolute paths and paths containing `..`.
- [ ] A minimal archive round-trip test passes.
- [ ] `scripts/spur-cargo test -p spur-notebook spur_app_archive -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `spur_app` module, `spur-notebook` dependency list, archive-specific tests.
- OUT of scope: MCP tool registration, Tauri file association, frontend UI, per-app installer generation.
- If archive logic needs changes outside `crates/spur-notebook`, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write failing manifest and path-safety tests**

Create `crates/spur-notebook/tests/spur_app_archive.rs` with tests shaped like:

```rust
use spur_notebook::spur_app::{
    is_safe_archive_path, SpurAppManifest, SPUR_APP_EXTENSION, SPUR_APP_MANIFEST,
    SPUR_APP_SCHEMA,
};

#[test]
fn manifest_defaults_to_spur_app_v1() {
    let manifest = SpurAppManifest::minimal("Forecast Dashboard", "app.ipynb");
    assert_eq!(SPUR_APP_EXTENSION, "spurapp");
    assert_eq!(SPUR_APP_MANIFEST, "spur-app.json");
    assert_eq!(manifest.schema, SPUR_APP_SCHEMA);
    assert_eq!(manifest.entry_notebook, "app.ipynb");
}

#[test]
fn archive_paths_reject_absolute_and_parent_segments() {
    assert!(is_safe_archive_path("app.ipynb"));
    assert!(is_safe_archive_path("widgets/sha256-abc.mjs"));
    assert!(!is_safe_archive_path("../app.ipynb"));
    assert!(!is_safe_archive_path("widgets/../../secret"));
    assert!(!is_safe_archive_path("/tmp/app.ipynb"));
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
scripts/spur-cargo test -p spur-notebook spur_app_archive -- --nocapture
```

Expected before implementation: compile failure because `spur_app` does not exist.

- [ ] **Step 3: Add core module and dependency**

Modify `crates/spur-notebook/Cargo.toml`:

```toml
zip = "2"
```

Modify `crates/spur-notebook/src/lib.rs`:

```rust
pub mod spur_app;
```

Create `crates/spur-notebook/src/spur_app.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

pub mod archive;

pub const SPUR_APP_EXTENSION: &str = "spurapp";
pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
    pub entry_notebook: String,
    pub open_mode: String,
    pub runtime: SpurAppRuntime,
    #[serde(default)]
    pub widgets: Vec<SpurAppWidgetAsset>,
    #[serde(default)]
    pub ports: Option<SpurAppPorts>,
    #[serde(default)]
    pub dependencies: SpurAppDependencies,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppRuntime {
    pub jute_min: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppWidgetAsset {
    pub module: String,
    #[serde(default)]
    pub css: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppPorts {
    #[serde(default)]
    pub include_snapshots: bool,
    #[serde(default)]
    pub manifest: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppDependencies {
    #[serde(default)]
    pub python: Option<String>,
    #[serde(default)]
    pub deno: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
    #[serde(default)]
    pub go: Option<String>,
}

impl SpurAppManifest {
    pub fn minimal(name: impl Into<String>, entry_notebook: impl Into<String>) -> Self {
        Self {
            schema: SPUR_APP_SCHEMA.to_string(),
            name: name.into(),
            entry_notebook: entry_notebook.into(),
            open_mode: "app".to_string(),
            runtime: SpurAppRuntime {
                jute_min: "0.1.0".to_string(),
                features: vec![
                    "frontend-cells".to_string(),
                    "anywidget-afm".to_string(),
                    "ports-arrow".to_string(),
                ],
            },
            widgets: Vec::new(),
            ports: None,
            dependencies: SpurAppDependencies::default(),
        }
    }
}

pub fn is_safe_archive_path(raw: &str) -> bool {
    let path = Path::new(raw);
    !raw.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
```

- [ ] **Step 4: Implement archive helpers**

Create `crates/spur-notebook/src/spur_app/archive.rs` with deterministic zip writer and reader helpers. At minimum expose:

```rust
pub fn write_entries<W, I>(writer: W, entries: I) -> Result<(), SpurAppArchiveError>
where
    W: std::io::Write + std::io::Seek,
    I: IntoIterator<Item = (String, Vec<u8>)>;

pub fn read_entry<R>(reader: R, path: &str) -> Result<Vec<u8>, SpurAppArchiveError>
where
    R: std::io::Read + std::io::Seek;
```

Use `is_safe_archive_path` on every entry name. Return a crate-local `SpurAppArchiveError` with variants for zip errors, I/O errors, unsafe paths, missing manifest, and invalid manifest JSON.

- [ ] **Step 5: Run focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook spur_app_archive -- --nocapture
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/Cargo.toml crates/spur-notebook/src/lib.rs crates/spur-notebook/src/spur_app.rs crates/spur-notebook/src/spur_app/archive.rs crates/spur-notebook/tests/spur_app_archive.rs
git commit -m "feat(spur-notebook): SPURAPP add archive schema"
```

---

### Task 2: Export and Import Service Functions

**Task ID:** `task-2-export-import-service`

**Files:**
- Modify: `crates/spur-notebook/src/spur_app.rs`
- Modify: `crates/spur-notebook/src/spur_app/archive.rs`
- Modify: `crates/spur-notebook/tests/spur_app_archive.rs`

**Depends on:** `task-1-core-schema-archive`

**Acceptance Criteria:**
- [ ] `export_spur_app` writes a `.spurapp` archive containing `spur-app.json` and `app.ipynb`.
- [ ] `import_spur_app` safely extracts into a caller-provided cache root and returns the embedded notebook path.
- [ ] A package exported on one machine can be imported from only the `.spurapp` file on another machine with Jute installed.
- [ ] Optional asset files are copied under `widgets/sha256-<hash>.<ext>` and listed in the manifest.
- [ ] Optional dependency lock files are copied under `env/` when present.
- [ ] Import preflight reports missing dependency locks without failing the import.
- [ ] `scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: core service functions and archive tests.
- OUT of scope: MCP tool parameter parsing, Tauri launch handling, frontend UI.
- If you need to change notebook store internals or kernel provisioning, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write failing export/import tests**

Extend `crates/spur-notebook/tests/spur_app_archive.rs`:

```rust
use std::fs;

use spur_notebook::spur_app::{
    export_spur_app, import_spur_app, SpurAppExportOptions, SPUR_APP_MANIFEST,
};

#[test]
fn export_and_import_minimal_spurapp_round_trips_notebook() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("source.ipynb");
    let output_path = temp.path().join("forecast.spurapp");
    let import_root = temp.path().join("cache");
    fs::write(
        &notebook_path,
        r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
    )
    .expect("seed notebook");

    let exported = export_spur_app(SpurAppExportOptions {
        notebook_path: notebook_path.clone(),
        output_path: output_path.clone(),
        name: Some("Forecast Dashboard".to_string()),
        widget_assets: Vec::new(),
        include_port_snapshots: false,
        dependency_roots: vec![temp.path().to_path_buf()],
    })
    .expect("export");

    assert_eq!(exported.manifest_path, SPUR_APP_MANIFEST);
    let imported = import_spur_app(&output_path, &import_root).expect("import");
    assert!(imported.notebook_path.exists());
    assert_eq!(fs::read_to_string(imported.notebook_path).unwrap(), fs::read_to_string(notebook_path).unwrap());
}
```

- [ ] **Step 2: Run focused failing test**

Run:

```bash
scripts/spur-cargo test -p spur-notebook export_and_import_minimal_spurapp_round_trips_notebook -- --nocapture
```

Expected before implementation: compile failure because export/import APIs do not exist.

- [ ] **Step 3: Add service types**

Add these public service types in `spur_app.rs`:

```rust
#[derive(Debug, Clone)]
pub struct SpurAppExportOptions {
    pub notebook_path: std::path::PathBuf,
    pub output_path: std::path::PathBuf,
    pub name: Option<String>,
    pub widget_assets: Vec<std::path::PathBuf>,
    pub include_port_snapshots: bool,
    pub dependency_roots: Vec<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpurAppExported {
    pub output_path: std::path::PathBuf,
    pub manifest_path: String,
    pub asset_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSpurApp {
    pub root: std::path::PathBuf,
    pub notebook_path: std::path::PathBuf,
    pub manifest: SpurAppManifest,
    pub preflight: SpurAppPreflight,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpurAppPreflight {
    pub missing_dependency_locks: Vec<String>,
    pub warnings: Vec<String>,
}
```

- [ ] **Step 4: Implement export/import**

Rules:
- `export_spur_app` reads the source `.ipynb` and stores it as `app.ipynb`.
- `name` defaults to the source notebook file stem, falling back to `"SpurApp"`.
- Widget assets are hashed with `blake3`; archive names are `widgets/sha256-<hex>.<ext>`.
- Dependency roots are scanned only for these exact filenames: `uv.lock`, `requirements.txt`, `deno.json`, `deno.lock`, `Cargo.lock`, `go.mod`, `go.sum`.
- `include_port_snapshots` is accepted but only includes snapshots when a `ports/manifest.json` sibling exists next to the notebook; otherwise add a warning, do not fail.
- `import_spur_app` extracts under a deterministic child of `cache_root`, such as `cache_root/<blake3 archive hash>/`.
- `import_spur_app` never writes outside `cache_root`.

- [ ] **Step 5: Run focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/spur_app.rs crates/spur-notebook/src/spur_app/archive.rs crates/spur-notebook/tests/spur_app_archive.rs
git commit -m "feat(spur-notebook): SPURAPP add import export services"
```

---

### Task 3: MCP Export and Import Tools

**Task ID:** `task-3-mcp-tools`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/export_spur_app.rs`
- Create: `crates/spur-notebook/src/mcp/tools/import_spur_app.rs`
- Create: `crates/spur-notebook/tests/spur_app_mcp_tools.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs`

**Depends on:** `task-2-export-import-service`

**Acceptance Criteria:**
- [ ] MCP tool list includes `notebook_export_spur_app`.
- [ ] MCP tool list includes `notebook_import_spur_app`.
- [ ] `.spurapp` paths are accepted only for the new SpurApp tools and still rejected for `.ipynb`-only notebook tools.
- [ ] Export tool returns `{ ok, path, manifest, asset_count }`.
- [ ] Import tool returns `{ ok, notebook_path, manifest, preflight }`.
- [ ] Import tool can optionally open the imported notebook by reusing the existing daemon open path.
- [ ] `scripts/spur-cargo test -p spur-notebook spur_app_mcp -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: MCP tool modules, path validation, tool registration tests.
- OUT of scope: Tauri file associations, frontend menus, archive internals except type imports.
- If existing `validate_notebook_path` cannot support method-specific `.spurapp` exceptions cleanly, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write failing MCP registration tests**

Create `crates/spur-notebook/tests/spur_app_mcp_tools.rs`:

```rust
use spur_notebook::mcp::tools;

#[test]
fn tools_include_spur_app_import_export() {
    let names = tools::tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "notebook_export_spur_app"));
    assert!(names.iter().any(|name| name == "notebook_import_spur_app"));
}
```

- [ ] **Step 2: Run focused failing test**

Run:

```bash
scripts/spur-cargo test -p spur-notebook tools_include_spur_app_import_export -- --nocapture
```

Expected before implementation: failure because tools are not registered.

- [ ] **Step 3: Add tool modules**

Create `export_spur_app.rs` with params:

```rust
#[derive(Debug, serde::Deserialize)]
struct ExportSpurAppParams {
    notebook_path: String,
    output_path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    widget_assets: Vec<String>,
    #[serde(default)]
    include_port_snapshots: bool,
}
```

Create `import_spur_app.rs` with params:

```rust
#[derive(Debug, serde::Deserialize)]
struct ImportSpurAppParams {
    path: String,
    #[serde(default)]
    open: bool,
}
```

Tool behavior:
- `notebook_export_spur_app` requires `notebook_path` to be `.ipynb` and `output_path` to be `.spurapp`.
- `notebook_import_spur_app` requires `path` to be `.spurapp`.
- `open: true` uses the same daemon open route as `notebook_open`; do not duplicate window/open logic.
- Both tools return structured JSON and map service errors to `McpError::invalid_params` for invalid input and internal errors for I/O/archive failures.

- [ ] **Step 4: Register tools and validation exceptions**

Modify `tools()` in `crates/spur-notebook/src/mcp/tools/mod.rs`:

```rust
export_spur_app::tool(),
import_spur_app::tool(),
```

Adjust path validation with a method-specific extension helper:

```rust
fn required_extension_for_method(method: &str) -> Option<&'static str> {
    match method {
        "notebook_export_spur_app.source" => Some("ipynb"),
        "notebook_export_spur_app.output" => Some("spurapp"),
        "notebook_import_spur_app" => Some("spurapp"),
        method if requires_ipynb_extension(method) => Some("ipynb"),
        _ => None,
    }
}
```

Use the repo's existing style; the snippet shows the intended behavior, not a required function name.

- [ ] **Step 5: Run focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook spur_app_mcp -- --nocapture
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/export_spur_app.rs crates/spur-notebook/src/mcp/tools/import_spur_app.rs crates/spur-notebook/src/mcp/tools/mod.rs crates/spur-notebook/tests/spur_app_mcp_tools.rs
git commit -m "feat(spur-notebook): SPURAPP add MCP tools"
```

---

### Task 4: Runtime `.spurapp` File Association Handling

**Task ID:** `task-4-file-association`

**Files:**
- Modify: `crates/spur-notebook/src/main.rs`
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/tauri.conf.json`

**Depends on:** `task-2-export-import-service`

**Acceptance Criteria:**
- [ ] Tauri declares `.spurapp` as a file association.
- [ ] Launching the runtime with a `.spurapp` argument imports the archive into the local cache and opens the embedded `app.ipynb`.
- [ ] A teammate with Jute installed can open a received `.spurapp` package without manually extracting it or finding the original source notebook.
- [ ] Launching with `.ipynb` keeps current behavior.
- [ ] Mixed launch arguments preserve order after resolving `.spurapp` into embedded notebook paths.
- [ ] `scripts/spur-cargo test -p spur-notebook file_association -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `src/main.rs` file argument resolution and Tauri file association config.
- OUT of scope: MCP tools, archive internals, frontend routing, broad Jute-to-Spur runtime rename.
- If handling `.spurapp` requires changes to `jute::window::open_notebook_path`, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add pure helper tests**

In `crates/spur-notebook/src/main.rs` tests, add a helper-level test. The worker may name the helper differently, but it should be pure enough to test without a Tauri `AppHandle`.

```rust
#[test]
fn app_mode_collects_spurapp_file_args() {
    let Mode::App { files, socket } = parse_mode_from([
        "--socket".to_string(),
        "/tmp/notebook-session.sock".to_string(),
        "forecast.spurapp".to_string(),
        "notes.ipynb".to_string(),
    ]) else {
        panic!("expected app mode");
    };

    assert_eq!(socket, Some(PathBuf::from("/tmp/notebook-session.sock")));
    assert_eq!(files, vec![PathBuf::from("forecast.spurapp"), PathBuf::from("notes.ipynb")]);
}
```

Add a second pure test after introducing the resolver:

```rust
#[test]
fn resolve_file_association_keeps_ipynb_and_imports_spurapp() {
    // Seed a minimal .spurapp with spur_notebook::spur_app::export_spur_app,
    // resolve it through the new helper, and assert the returned path ends in app.ipynb.
}
```

The second test must use a real temp archive; do not mock the extension check only.

- [ ] **Step 2: Run failing tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook file_association -- --nocapture
```

Expected before implementation: failure for the new resolver test.

- [ ] **Step 3: Implement file resolution**

Add a helper in `main.rs`:

```rust
fn resolve_file_association_targets(files: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut targets = Vec::new();
    for file in files {
        if file.extension().and_then(|ext| ext.to_str()) == Some("spurapp") {
            let cache_root = spur_notebook::spur_app::default_import_cache_root()?;
            let imported = spur_notebook::spur_app::import_spur_app(file, &cache_root)
                .with_context(|| format!("failed to import {}", file.display()))?;
            targets.push(imported.notebook_path);
        } else {
            targets.push(file.clone());
        }
    }
    Ok(targets)
}
```

Call this from `handle_file_associations` before `jute::window::open_notebook_path`.

- [ ] **Step 4: Add Tauri association**

Modify `tauri.conf.json` file associations so `ext` includes both `"ipynb"` and `"spurapp"`, and use a description that names SpurApp without renaming the whole Jute runtime:

```json
{
  "ext": ["ipynb", "spurapp"],
  "mimeType": "application/x-spurapp+zip",
  "name": "SpurApp Package",
  "description": "SPUR notebook application package"
}
```

If Tauri requires one association per MIME type, keep the existing `.ipynb` entry and add a second `.spurapp` entry instead.

- [ ] **Step 5: Run focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook file_association -- --nocapture
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/main.rs crates/spur-notebook/jute-notebook/src-tauri/tauri.conf.json
git commit -m "feat(spur-notebook): SPURAPP open spurapp packages"
```

---

### Task 5: Integration Verification and Documentation

**Task ID:** `task-5-integration-docs`

**Files:**
- Modify: `docs/superpowers/specs/2026-06-06-spur-app-packaging-delivery-research.ipynb`
- Create: `docs/superpowers/plans/2026-06-06-spur-app-packaging-delivery.md` if this committed plan is absent in the worker worktree
- Modify: any directly relevant README or command documentation found in `crates/spur-notebook` only if it already documents notebook MCP tools.

**Depends on:** `task-3-mcp-tools`, `task-4-file-association`

**Acceptance Criteria:**
- [ ] No implementation/docs use legacy package artifact spelling for the new package artifact.
- [ ] `SpurApp`, `.spurapp`, and `spur-app.json` are used consistently.
- [ ] Research notebook still explains that Jute is the runtime shell and SpurApp is the package artifact.
- [ ] Final verification covers the handoff flow: export local notebook to `.spurapp`, import/open only that package, and confirm the embedded notebook path plus manifest/preflight are returned.
- [ ] Focused archive/MCP/file-association tests pass.
- [ ] `scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture` passes.
- [ ] `scripts/spur-cargo test -p spur-notebook spur_app_mcp -- --nocapture` passes.
- [ ] `scripts/spur-cargo test -p spur-notebook file_association -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: final naming cleanup, docs touching only the SpurApp packaging feature, focused verification.
- OUT of scope: broad Jute runtime rename, UI redesign, per-app installer generation, unrelated dirty files.
- If broad docs or runtime names need changing to satisfy the tests, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Scan for forbidden package names in touched scope**

Run:

```bash
rg -n "jute""app|jute-app[.]json|[.]jute""app" crates/spur-notebook docs/superpowers/specs/2026-06-06-spur-app-packaging-delivery-research.ipynb
```

Expected: no matches referring to the new package artifact. Existing `Jute-App` mentions in the original source spec are allowed only when explicitly referring to the older design notebook title.

- [ ] **Step 2: Verify focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture
scripts/spur-cargo test -p spur-notebook spur_app_mcp -- --nocapture
scripts/spur-cargo test -p spur-notebook file_association -- --nocapture
```

Expected: all pass.

- [ ] **Step 3: Update docs only if needed**

If `crates/spur-notebook` already contains README or command documentation for notebook MCP tools, add concise entries:

```text
notebook_export_spur_app: export an .ipynb notebook into a .spurapp package.
notebook_import_spur_app: import a .spurapp package and optionally open its embedded notebook.
```

Do not create broad marketing docs.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-06-spur-app-packaging-delivery-research.ipynb docs/superpowers/plans/2026-06-06-spur-app-packaging-delivery.md crates/spur-notebook
git commit -m "docs(spur-notebook): SPURAPP document packaging delivery"
```

---

## Self-Review

**Spec coverage:** The plan covers the package handoff contract: local notebook development, `.spurapp` export, teammate import/open through an installed Jute runtime, embedded notebook launch, AFM asset packaging as content-hashed files, dependency lock collection/preflight, optional port snapshots, MCP import/export, file association, and naming cleanup. It intentionally defers per-app Tauri installer generation and full frontend widget-loader changes because those are later delivery tiers in the research notebook.

**Placeholder scan:** No `TBD`, `TODO`, or unspecified implementation steps remain. The one flexible point is the exact helper function name in file association, but the required behavior and tests are explicit.

**Type consistency:** `SpurAppManifest`, `SpurAppExportOptions`, `SpurAppExported`, `ImportedSpurApp`, and `SpurAppPreflight` are defined before later tasks depend on them.

**DAG validation:** The graph is acyclic. Tasks 3 and 4 can proceed in parallel after Task 2. Task 5 depends on both integration surfaces.

**beads compatibility:** Every task has a unique ID, explicit dependencies, acceptance criteria, worker routing, and scope boundaries.

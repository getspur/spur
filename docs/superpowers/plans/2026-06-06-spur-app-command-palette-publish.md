# Spur App Command Palette Publish Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-06-spur-app-packaging-delivery-research.ipynb`
**Design basis:** Completed packaging delivery plan `f0e84753-c0a9-4f59-ab1f-40cd3f7cee03`

**Goal:** Add a notebook command-palette action that publishes the current notebook as a shareable `.spurapp` package.

**Architecture:** Reuse the existing `spur_app::export_spur_app` archive service. Add a narrow Tauri command for frontend callers, then add a `cmdk` command item that saves the active notebook, asks for a destination `.spurapp` file, and invokes the backend export command.

**Tech Stack:** Rust/Tauri commands, `spur_app` archive service, React, `cmdk`, `@tauri-apps/plugin-dialog`, Vitest/Testing Library.

---

### Task 1: Backend Tauri Publish Command

**Task ID:** `t1-backend-publish`

**Files:**
- Modify: `crates/spur-notebook/src/commands.rs`
- Modify: `crates/spur-notebook/src/main.rs`
- Test: `crates/spur-notebook/tests/spur_app_publish_command.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] A new public Tauri command named `publish_spur_app` exports a source `.ipynb` to a destination `.spurapp`.
- [ ] The command reuses `crate::spur_app::export_spur_app` and defaults `dependency_roots` to the source notebook parent directory, matching the MCP export tool behavior.
- [ ] The command returns structured data with `path`, `manifest`, `asset_count`, and `preflight`.
- [ ] The command is registered in the Tauri invoke handler.
- [ ] Rust tests prove the helper writes a valid `.spurapp` and includes dependency locks beside the source notebook.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `commands.rs` export wrapper, `main.rs` handler registration, focused Rust test.
- OUT of scope: command palette UI, standalone CLI, package signing, registry upload, installer publishing.
- If command implementation requires changing `spur_app` archive schema or MCP tool behavior, emit `scope_drift` before editing those files.

**Implementation:**
- [ ] **Step 1: Add the failing test**

Create `crates/spur-notebook/tests/spur_app_publish_command.rs` with a test shaped like:

```rust
use std::fs;

use spur_notebook::commands::publish_spur_app_for_paths;
use spur_notebook::spur_app::archive;

#[test]
fn publish_command_exports_package_with_dependency_locks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("forecast.ipynb");
    let output_path = temp.path().join("dist").join("forecast.spurapp");
    fs::write(
        &notebook_path,
        r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
    )
    .expect("write notebook");
    fs::write(temp.path().join("requirements.txt"), "anywidget==0.9.18\n")
        .expect("write requirements");

    let response = publish_spur_app_for_paths(
        notebook_path.clone(),
        output_path.clone(),
        Some("Forecast Dashboard".to_string()),
        false,
    )
    .expect("publish spur app");

    assert_eq!(response.path, output_path.to_string_lossy().to_string());
    assert_eq!(response.asset_count, 0);
    assert_eq!(response.manifest.name, "Forecast Dashboard");
    assert_eq!(response.manifest.entry_notebook, "app.ipynb");
    assert_eq!(response.manifest.dependencies.python.as_deref(), Some("env/requirements.txt"));

    let manifest = archive::read_manifest(fs::File::open(output_path).expect("open package"))
        .expect("read manifest");
    assert_eq!(manifest.name, "Forecast Dashboard");
    assert_eq!(manifest.dependencies.python.as_deref(), Some("env/requirements.txt"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
scripts/spur-cargo test -p spur-notebook --test spur_app_publish_command -- --nocapture
```

Expected: FAIL because `publish_spur_app_for_paths` does not exist.

- [ ] **Step 3: Add the command and helper**

In `crates/spur-notebook/src/commands.rs`, import the archive service:

```rust
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};

use crate::spur_app::{self, archive, SpurAppExportOptions, SpurAppManifest, SpurAppPreflight};
```

Add response and helper types near the existing command response structs:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishSpurAppResponse {
    pub path: String,
    pub manifest: SpurAppManifest,
    pub asset_count: usize,
    pub preflight: SpurAppPreflight,
}
```

Add a testable helper plus Tauri command:

```rust
pub fn publish_spur_app_for_paths(
    notebook_path: PathBuf,
    output_path: PathBuf,
    name: Option<String>,
    include_port_snapshots: bool,
) -> Result<PublishSpurAppResponse, String> {
    let dependency_roots = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();

    let exported = spur_app::export_spur_app(SpurAppExportOptions {
        notebook_path,
        output_path,
        name,
        widget_assets: Vec::new(),
        include_port_snapshots,
        dependency_roots,
    })
    .map_err(|error| error.to_string())?;

    let manifest = archive::read_manifest(
        std::fs::File::open(&exported.output_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    Ok(PublishSpurAppResponse {
        path: exported.output_path.to_string_lossy().to_string(),
        manifest,
        asset_count: exported.asset_count,
        preflight: exported.preflight,
    })
}

#[tauri::command]
pub async fn publish_spur_app(
    notebook_path: String,
    output_path: String,
    name: Option<String>,
    include_port_snapshots: Option<bool>,
) -> Result<PublishSpurAppResponse, String> {
    publish_spur_app_for_paths(
        PathBuf::from(notebook_path),
        PathBuf::from(output_path),
        name,
        include_port_snapshots.unwrap_or(false),
    )
}
```

If `SpurAppManifest` or `SpurAppPreflight` already serialize in camelCase differently, do not rename their fields in this task; return the existing Rust serialization so MCP and UI stay aligned.

- [ ] **Step 4: Register the command**

In `crates/spur-notebook/src/main.rs`, add this to `tauri::generate_handler!` beside the existing `spur_notebook::commands::*` entries:

```rust
spur_notebook::commands::publish_spur_app,
```

- [ ] **Step 5: Run focused Rust verification**

Run:

```bash
scripts/spur-cargo test -p spur-notebook --test spur_app_publish_command -- --nocapture
scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/commands.rs crates/spur-notebook/src/main.rs crates/spur-notebook/tests/spur_app_publish_command.rs
git commit -m "feat(spur-notebook): add spur app publish command"
```

---

### Task 2: Frontend Command Palette Publish Flow

**Task ID:** `t2-palette-publish`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/publishSpurApp.ts`
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx`

**Depends on:** `t1-backend-publish`

**Acceptance Criteria:**
- [ ] The command palette exposes `Publish Spur App...` in the Notebook group and disables it when the notebook has no saved path.
- [ ] Selecting the command saves the active notebook with `notebook.saveNow()` before export.
- [ ] The flow opens a save dialog filtered to `.spurapp`, defaulting to the current notebook sibling path with a `.spurapp` extension.
- [ ] If the user cancels the save dialog, no backend publish command is invoked.
- [ ] If the user picks a destination, the frontend invokes `publish_spur_app` with `notebookPath`, `outputPath`, `name`, and `includePortSnapshots: false`.
- [ ] Frontend tests cover disabled state, cancellation, and save-before-invoke ordering.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: command palette item, small publish helper, frontend tests.
- OUT of scope: toast system, registry upload, sharing UI, drag-and-drop import, app-mode runtime changes.
- If a success notification requires creating a global notification system, emit `scope_drift` and keep this task to command execution only.

**Implementation:**
- [ ] **Step 1: Write helper tests first**

Create `NotebookCommandMenu.test.tsx` using Testing Library patterns from existing notebook UI tests. Mock:

```ts
vi.mock("@/stores/notebook", () => ({ useNotebook: () => mocks.notebook }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: saveDialogMock }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
```

Use a mocked notebook object:

```ts
const notebook = {
  store: createStore<any>()(() => ({
    serverState: { cells: {} },
    viewState: { selectedCellId: null, path: "/tmp/forecast.ipynb" },
  })),
  saveNow: saveNowMock,
  execute: vi.fn(),
  interruptKernel: vi.fn(),
  restartKernel: vi.fn(),
  setCellType: vi.fn(),
};
```

Include these tests:

```ts
test("publish command is disabled until the notebook has a path", async () => {
  mocks.notebook = notebookWithPath(null);
  render(<NotebookCommandMenu />);
  fireEvent.keyDown(document, { key: "k", metaKey: true });
  expect(screen.getByText("Publish Spur App...").closest("[cmdk-item]"))
    .toHaveAttribute("aria-disabled", "true");
});

test("publish command saves before invoking backend export", async () => {
  const order: string[] = [];
  saveNowMock.mockImplementation(async () => order.push("save"));
  saveDialogMock.mockResolvedValue("/tmp/forecast.spurapp");
  invokeMock.mockImplementation(async () => order.push("invoke"));

  mocks.notebook = notebookWithPath("/tmp/forecast.ipynb");
  render(<NotebookCommandMenu />);
  fireEvent.keyDown(document, { key: "k", metaKey: true });
  fireEvent.click(screen.getByText("Publish Spur App..."));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("publish_spur_app", {
    notebookPath: "/tmp/forecast.ipynb",
    outputPath: "/tmp/forecast.spurapp",
    name: "forecast",
    includePortSnapshots: false,
  }));
  expect(order).toEqual(["save", "invoke"]);
});

test("publish command stops after cancelled save dialog", async () => {
  saveDialogMock.mockResolvedValue(null);
  mocks.notebook = notebookWithPath("/tmp/forecast.ipynb");
  render(<NotebookCommandMenu />);
  fireEvent.keyDown(document, { key: "k", metaKey: true });
  fireEvent.click(screen.getByText("Publish Spur App..."));
  await waitFor(() => expect(saveNowMock).toHaveBeenCalled());
  expect(invokeMock).not.toHaveBeenCalled();
});
```

Adjust the DOM assertion for disabled items to match `cmdk` output if it differs, but keep the behavioral intent intact.

- [ ] **Step 2: Run frontend test to verify it fails**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx
```

Expected: FAIL because the publish helper and palette item do not exist.

- [ ] **Step 3: Add `publishSpurApp.ts`**

Create a small helper:

```ts
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";

type PublishableNotebook = {
  saveNow: () => Promise<void>;
};

export type PublishSpurAppResponse = {
  path: string;
  manifest: unknown;
  assetCount: number;
  preflight: unknown;
};

export function defaultSpurAppPath(notebookPath: string): string {
  return notebookPath.replace(/\.ipynb$/i, ".spurapp");
}

export function defaultSpurAppName(notebookPath: string): string {
  const file = notebookPath.split(/[\\/]/).pop() ?? "Spur App";
  return file.replace(/\.ipynb$/i, "") || "Spur App";
}

export async function publishSpurApp(
  notebook: PublishableNotebook,
  notebookPath: string,
): Promise<PublishSpurAppResponse | null> {
  await notebook.saveNow();
  const outputPath = await save({
    title: "Publish Spur App",
    defaultPath: defaultSpurAppPath(notebookPath),
    filters: [{ name: "Spur App", extensions: ["spurapp"] }],
  });
  if (!outputPath) return null;

  return await invoke<PublishSpurAppResponse>("publish_spur_app", {
    notebookPath,
    outputPath,
    name: defaultSpurAppName(notebookPath),
    includePortSnapshots: false,
  });
}
```

- [ ] **Step 4: Wire the command palette item**

In `NotebookCommandMenu.tsx`:

```ts
import { PackageIcon } from "lucide-react";
import { publishSpurApp } from "./publishSpurApp";
```

Add a callback:

```ts
const publishCurrentNotebook = useCallback(() => {
  if (!notebookPath) return;
  closeAndRun(() => {
    void publishSpurApp(notebook, notebookPath).catch((error) => {
      console.error("Failed to publish Spur App", error);
    });
  });
}, [closeAndRun, notebook, notebookPath]);
```

Add the item under the `Notebook` group:

```tsx
<Command.Item disabled={!notebookPath} onSelect={publishCurrentNotebook}>
  <PackageIcon />
  Publish Spur App...
</Command.Item>
```

- [ ] **Step 5: Run focused frontend verification**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx
scripts/spur-pnpm run typecheck
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/publishSpurApp.ts
git commit -m "feat(spur-notebook): add publish command palette action"
```

---

### Task 3: End-to-End Verification Pass

**Task ID:** `t3-verify`

**Files:**
- Modify: `docs/superpowers/plans/2026-06-06-spur-app-command-palette-publish.md`

**Depends on:** `t1-backend-publish`, `t2-palette-publish`

**Acceptance Criteria:**
- [ ] Focused backend and frontend verification commands pass against the integrated plan base.
- [ ] The plan document records the final verification commands and outcomes.
- [ ] No new broad packaging scope is introduced.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: run focused verification, append a concise verification note to this plan.
- OUT of scope: feature implementation changes unless a test failure requires a minimal fix inside Task 1 or Task 2 files.
- If verification reveals cross-task integration conflict, emit `scope_drift` with the failing command and file list before broad edits.

**Implementation:**
- [ ] **Step 1: Run integrated verification**

Run:

```bash
scripts/spur-cargo test -p spur-notebook --test spur_app_publish_command -- --nocapture
scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture
scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx
scripts/spur-pnpm run typecheck
```

Expected: PASS.

- [ ] **Step 2: Append verification note**

Append a section to this file:

```markdown
---

## Verification

- `scripts/spur-cargo test -p spur-notebook --test spur_app_publish_command -- --nocapture` - passed
- `scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture` - passed
- `scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx` - passed
- `scripts/spur-pnpm run typecheck` - passed
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/plans/2026-06-06-spur-app-command-palette-publish.md
git commit -m "docs(spur-notebook): record publish command verification"
```

---

## DAG

```text
t1-backend-publish
  -> t2-palette-publish
t1-backend-publish
  -> t3-verify
t2-palette-publish
  -> t3-verify
```

## Self-Review

- Spec coverage: The existing packaging/export service is reused; this plan adds only the missing command-palette entry point for local `.spurapp` publishing.
- Placeholder scan: No TBD/TODO implementation placeholders remain in task instructions.
- Type consistency: Frontend invoke keys use camelCase arguments (`notebookPath`, `outputPath`, `includePortSnapshots`) matching Tauri's argument conversion for the Rust `publish_spur_app` parameters.
- DAG validation: Task 1 provides the backend command; Task 2 depends on that command contract; Task 3 depends on both for integrated verification.

---

## Verification

Integrated verification was attempted after Tasks 1 and 2 were approved. The
Rust Spur App packaging checks passed; the exact frontend remote-default checks
failed for known shared frontend/VM environment reasons outside this packaging
feature and are accepted residual verification caveats for this plan.

- `scripts/spur-cargo test -p spur-notebook --test spur_app_publish_command -- --nocapture` - passed.
- `scripts/spur-cargo test -p spur-notebook spur_app -- --nocapture` - passed.
- `scripts/spur-pnpm test -- src/ui/notebook/NotebookCommandMenu.test.tsx` - failed before Vitest collected tests because the remote `/mnt/cargo/pnpm-nm` dependency resolution environment could not resolve `@testing-library/jest-dom/vitest`; this happened before or outside the task-specific publish-flow code.
- `scripts/spur-pnpm run typecheck` - failed with broad existing frontend TypeScript errors unrelated to this diff.

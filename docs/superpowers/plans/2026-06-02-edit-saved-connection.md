# Edit Saved Connection Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-02-edit-saved-connection-design.md`
**Design epic:** _(none — brainstorming spec approved inline)_

**Goal:** Let a user edit an existing saved API connection by reloading its configuration into `AddRestApiWizard` (same journey, fields pre-filled) and saving the changes back to the global connection store.

**Architecture:** Reuse `AddRestApiWizard` with an optional `editConnection` prop (Source step skipped, name read-only, fields pre-filled from the `ConnectionTemplate`). Persist via a new additive `DaemonControlCommand::UpdateSavedConnection` that regenerates the manifest only when a new spec is supplied, otherwise preserves the stored manifest/tables. Routes to the existing `connection_store::upsert` (overwrite-by-name, preserves `createdAt`).

**Tech Stack:** Rust (tauri daemon, ts-rs), TypeScript/React (jute-notebook UI), vitest, cargo test.

---

## File Structure Mapping

| File | Responsibility | Task |
|------|----------------|------|
| `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` | New `UpdateSavedConnection` enum variant + round-trip test | task-1 |
| `crates/spur-notebook/src/mcp/mod.rs` | `update_saved_connection` handler (real + stub) + dispatch arm | task-2 |
| `crates/spur-notebook/tests/api_datasource_import_e2e.rs` | Edit round-trip e2e tests | task-2 |
| `crates/spur-notebook/jute-notebook/src/bindings/*` | Regenerated ts-rs bindings (incl. `DaemonControlCommand`) | task-3 |
| `crates/spur-notebook/jute-notebook/src/daemon/control.ts` | `updateSavedConnectionCommand` builder + Input type | task-3 |
| `crates/spur-notebook/jute-notebook/src/daemon/control.test.ts` | Builder shape test | task-3 |
| `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx` | Edit-mode prop, prefill, submit routing | task-4 |
| `crates/spur-notebook/jute-notebook/src/ui/notebook/DatasourceSidebar.tsx` | Edit button + editingConnection state | task-5 |

## Dependency DAG

```
task-1 ──┬──▶ task-2  (backend handler + e2e)
         └──▶ task-3 ──▶ task-4 ──▶ task-5
```

task-2 and task-3 run in parallel after task-1. The frontend chain (3→4→5) is sequential because each consumes the prior's interface.

---

### Task 1: Add `UpdateSavedConnection` daemon command variant

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (enum `DaemonControlCommand`, after the `DeleteSavedConnection` variant ~line 318; tests module ~line 2010)

**Depends on:** none

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `DaemonControlCommand::UpdateSavedConnection { name, spec_text, credentials }` exists.
- [ ] Serializes with `command: "update_saved_connection"`, snake_case fields.
- [ ] `credentials` defaults to `[]` when omitted; round-trip test passes.
- [ ] `cargo build -p jute` succeeds.

**Scope Boundary:**
- IN scope: the enum variant + one test in `commands.rs`.
- OUT of scope: the handler (task-2), bindings regen (task-3). Do NOT edit `mcp/mod.rs`.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add the variant** to `DaemonControlCommand` immediately after `DeleteSavedConnection { name: String },`:

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

- [ ] **Step 2: Add a round-trip test** in the `tests` module (mirror `attach_saved_connection_command_defaults_and_round_trips_credentials`):

```rust
#[test]
fn update_saved_connection_command_round_trips() {
    let json = serde_json::json!({
        "command": "update_saved_connection",
        "name": "stripe_reporting",
        "spec_text": null,
        "credentials": [["STRIPE_API_KEY", "sk_live_x"]],
    });
    let cmd: DaemonControlCommand =
        serde_json::from_value(json.clone()).expect("deserializes");
    match &cmd {
        DaemonControlCommand::UpdateSavedConnection {
            name,
            spec_text,
            credentials,
        } => {
            assert_eq!(name, "stripe_reporting");
            assert!(spec_text.is_none());
            assert_eq!(credentials.len(), 1);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
    // credentials defaults to [] when omitted
    let minimal: DaemonControlCommand = serde_json::from_value(serde_json::json!({
        "command": "update_saved_connection",
        "name": "x",
        "spec_text": "openapi: 3.0.0",
    }))
    .expect("deserializes without credentials");
    assert!(matches!(
        minimal,
        DaemonControlCommand::UpdateSavedConnection { ref credentials, .. } if credentials.is_empty()
    ));
}
```

- [ ] **Step 3: Verify** — `cargo test -p jute update_saved_connection_command_round_trips` passes; `cargo build -p jute` clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs
git commit -m "feat(spur-notebook): add UpdateSavedConnection daemon command variant"
```

---

### Task 2: Implement the `update_saved_connection` handler

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arm ~line 1386; real handler near `attach_saved_connection` ~line 1641; `#[cfg(not(...))]` stub near line 2035)
- Test: `crates/spur-notebook/tests/api_datasource_import_e2e.rs`

**Depends on:** task-1

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `UpdateSavedConnection` is dispatched to `update_saved_connection`.
- [ ] With `spec_text = None`: stored `manifest_toml` + `tables` preserved; `created_at` unchanged; `updated_at` advanced.
- [ ] With `spec_text = Some(spec)`: manifest + tables regenerated.
- [ ] Unknown name → error code `saved_connection_not_found`.
- [ ] Returns the `AttachedSavedConnection` payload (`{ entry, missing_env_vars }`); emits `connections://changed`.
- [ ] `cargo test -p spur-notebook --test api_datasource_import_e2e` passes.

**Scope Boundary:**
- IN scope: handler + dispatch arm in `mcp/mod.rs`; new e2e tests.
- OUT of scope: `commands.rs` (task-1), any frontend file.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add the dispatch arm** after the `DeleteSavedConnection` arm (~line 1388):

```rust
            DaemonControlCommand::UpdateSavedConnection {
                name,
                spec_text,
                credentials,
            } => self.update_saved_connection(name, spec_text, credentials).await,
```

- [ ] **Step 2: Add the real handler** (place it right after `attach_saved_connection`, before `delete_saved_connection`):

```rust
    #[cfg(feature = "datasource-introspect")]
    async fn update_saved_connection(
        &self,
        name: String,
        spec_text: Option<String>,
        credentials: Vec<(String, String)>,
    ) -> Result<DaemonControlSuccess, BridgeError> {
        use spur_rest_table_gateway::adapter::Adapter as _;

        // Load the edit target. `name` is the store key (locked in the UI).
        let existing = crate::connection_store::list()
            .await
            .map_err(|error| BridgeError::Handler {
                code: "saved_connections_list_failed".to_string(),
                message: error.to_string(),
            })?
            .into_iter()
            .find(|template| template.name == name)
            .ok_or_else(|| BridgeError::Handler {
                code: "saved_connection_not_found".to_string(),
                message: format!("no saved connection named {name}"),
            })?;

        // Provided credential values inject into the session env only (never persisted).
        let supplied_env_vars = credentials
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for (key, value) in &credentials {
            if !value.is_empty() {
                std::env::set_var(key, value);
            }
        }

        // Regenerate the manifest from a new spec, or keep the stored one.
        let manifest_toml = match spec_text {
            Some(spec_text) if !spec_text.trim().is_empty() => {
                let (_source, manifest) =
                    build_api_import_manifest(&name, existing.provider.clone(), Some(spec_text))?;
                let mut toml =
                    spur_rest_table_gateway::adapter::nango::manifest_to_toml(&manifest);
                toml.push_str(&spur_rest_table_gateway::adapter::openapi::tables_to_toml(
                    &manifest.tables,
                ));
                toml
            }
            _ => existing.manifest_toml.clone(),
        };

        let manifest =
            spur_rest_table_gateway::adapter::manifest::Manifest::from_toml(&manifest_toml)
                .map_err(|error| BridgeError::Handler {
                    code: "saved_connection_manifest_parse_failed".to_string(),
                    message: error.to_string(),
                })?;
        let adapter =
            spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter::new(manifest);
        let adapter_name = adapter.name().to_string();
        let tables = adapter
            .catalog()
            .into_iter()
            .map(|table| api_datasource_table(&adapter_name, table))
            .collect::<Vec<_>>();

        // Keep stored credential keys unless the edit supplied new ones.
        let credential_env_vars = if supplied_env_vars.is_empty() {
            existing.credential_env_vars.clone()
        } else {
            supplied_env_vars
        };
        let missing_env_vars = credential_env_vars
            .iter()
            .filter(|env_var| std::env::var_os(env_var).is_none())
            .cloned()
            .collect::<Vec<_>>();

        let entry = self
            .register_api_datasource_entry_inner(name.clone(), adapter_name, tables.clone())
            .await?;

        let template = crate::connection_store::ConnectionTemplate {
            name,
            provider: existing.provider,
            group: existing.group,
            manifest_toml,
            tables,
            credential_env_vars,
            created_at: existing.created_at,
            updated_at: chrono::Utc::now(),
        };
        if let Err(error) = crate::connection_store::upsert(template).await {
            warn!(%error, "failed to persist updated API connection template");
        }
        self.emit_connections_changed().await;

        let payload = json!({
            "entry": entry,
            "missing_env_vars": missing_env_vars,
        });
        saved_connection_result(
            jute::commands::DaemonControlResult::AttachedSavedConnection(payload),
            "saved_connection_update_encode_failed",
        )
    }
```

- [ ] **Step 3: Add the stub** alongside the other `#[cfg(not(feature = "datasource-introspect"))]` stubs (~line 2035):

```rust
    #[cfg(not(feature = "datasource-introspect"))]
    async fn update_saved_connection(
        &self,
        _name: String,
        _spec_text: Option<String>,
        _credentials: Vec<(String, String)>,
    ) -> Result<DaemonControlSuccess, BridgeError> {
        Err(BridgeError::Handler {
            code: "datasource_introspect_unavailable".to_string(),
            message: "datasource introspection is disabled".to_string(),
        })
    }
```

- [ ] **Step 4: Add e2e tests** in `api_datasource_import_e2e.rs`, mirroring the existing `saved_connection_list_attach_delete_roundtrip_reports_missing_env` setup (build a `NotebookDaemonControl`, import a connection first, then issue `update_saved_connection`). Assert:

```rust
// After importing "scores" with a spec and verifying it has tables:
// 1. Update with spec_text = None preserves manifest + tables, advances updated_at.
let before = connection_store::list().await.unwrap();
let original = before.iter().find(|t| t.name == "scores").unwrap().clone();

let resp = control
    .handle(update_request(json!({
        "command": "update_saved_connection",
        "name": "scores",
        "spec_text": null,
        "credentials": [],
    })))
    .await;
assert!(resp.ok, "update without spec succeeds: {:?}", resp.error);

let after = connection_store::list().await.unwrap();
let updated = after.iter().find(|t| t.name == "scores").unwrap();
assert_eq!(updated.manifest_toml, original.manifest_toml, "manifest preserved");
assert_eq!(updated.tables, original.tables, "tables preserved");
assert_eq!(updated.created_at, original.created_at, "created_at preserved");
assert!(updated.updated_at >= original.updated_at, "updated_at advanced");

// 2. Update an unknown name → saved_connection_not_found.
let missing = control
    .handle(update_request(json!({
        "command": "update_saved_connection",
        "name": "does_not_exist",
        "spec_text": null,
        "credentials": [],
    })))
    .await;
assert!(!missing.ok);
assert_eq!(missing.error.unwrap().code, "saved_connection_not_found");
```

> Note: reuse the file's existing request-construction helper (the same one the attach/delete roundtrip test uses to build a `DaemonControlRequest`). If that test builds requests inline rather than via a helper, build the `update_saved_connection` request the same inline way instead of inventing `update_request`.

- [ ] **Step 5: Verify** — `cargo test -p spur-notebook --test api_datasource_import_e2e` passes; `cargo build -p spur-notebook` clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/mcp/mod.rs crates/spur-notebook/tests/api_datasource_import_e2e.rs
git commit -m "feat(spur-notebook): handle UpdateSavedConnection (preserve tables unless re-specced)"
```

---

### Task 3: Regenerate bindings + add `updateSavedConnectionCommand` builder

**Task ID:** `task-3`

**Files:**
- Modify (generated): `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts` (via regen)
- Modify: `crates/spur-notebook/jute-notebook/src/daemon/control.ts`
- Test: `crates/spur-notebook/jute-notebook/src/daemon/control.test.ts`

**Depends on:** task-1

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] Bindings regenerated; `DaemonControlCommand` TS union includes the `update_saved_connection` variant.
- [ ] `updateSavedConnectionCommand(input)` builder + `UpdateSavedConnectionInput` type exported.
- [ ] `control.test.ts` covers the builder shape (with and without credentials).
- [ ] `pnpm vitest run src/daemon/control.test.ts` (or the repo's configured runner) passes; `tsc` clean.

**Scope Boundary:**
- IN scope: regen bindings, `control.ts`, `control.test.ts`.
- OUT of scope: UI components (task-4/5). Do NOT hand-edit `bindings/*` beyond what regen produces.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Regenerate bindings** (the `ts-rs-export` bin clears and rewrites `src/bindings`):

```bash
cd crates/spur-notebook/jute-notebook/src-tauri
cargo run --bin ts-rs-export
```

Confirm `git diff` on `crates/spur-notebook/jute-notebook/src/bindings/DaemonControlCommand.ts` shows the new `update_saved_connection` member and no unrelated churn.

- [ ] **Step 2: Add the command type + builder** in `control.ts` (near the `DeleteSavedConnection` definitions, ~line 51 and ~line 166):

```ts
export type UpdateSavedConnectionCommand = Extract<
  DaemonControlCommand,
  { command: "update_saved_connection" }
>;
export type UpdateSavedConnectionInput = Omit<
  UpdateSavedConnectionCommand,
  "command" | "credentials"
> & {
  credentials?: [string, string][];
};
```

```ts
export function updateSavedConnectionCommand(
  input: UpdateSavedConnectionInput,
): UpdateSavedConnectionCommand {
  return {
    command: "update_saved_connection",
    name: input.name,
    spec_text: input.spec_text,
    credentials: input.credentials ?? [],
  };
}
```

> The response is decoded with the existing `attachedSavedConnectionFromDaemonControlResponse` — no new decoder needed.

- [ ] **Step 3: Add a builder test** in `control.test.ts` (mirror the existing `attachSavedConnectionCommand` expectations):

```ts
import { updateSavedConnectionCommand } from "./control";

it("builds update_saved_connection with credentials", () => {
  expect(
    updateSavedConnectionCommand({
      name: "scores",
      spec_text: null,
      credentials: [["SCORES_API_KEY", "x"]],
    }),
  ).toEqual({
    command: "update_saved_connection",
    name: "scores",
    spec_text: null,
    credentials: [["SCORES_API_KEY", "x"]],
  });
});

it("defaults update_saved_connection credentials to []", () => {
  expect(
    updateSavedConnectionCommand({ name: "scores", spec_text: "openapi: 3.0.0" }),
  ).toEqual({
    command: "update_saved_connection",
    name: "scores",
    spec_text: "openapi: 3.0.0",
    credentials: [],
  });
});
```

- [ ] **Step 4: Verify** — run the project's vitest + typecheck (e.g. `pnpm -C crates/spur-notebook/jute-notebook test` and `pnpm -C crates/spur-notebook/jute-notebook tsc --noEmit`, matching whatever scripts the package defines). Tests green, no type errors.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/bindings crates/spur-notebook/jute-notebook/src/daemon/control.ts crates/spur-notebook/jute-notebook/src/daemon/control.test.ts
git commit -m "feat(jute-notebook): updateSavedConnectionCommand builder + regen bindings"
```

---

### Task 4: Edit mode in `AddRestApiWizard`

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`

**Depends on:** task-3

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] New optional prop `editConnection?: ConnectionTemplate | null`; default behavior unchanged when absent.
- [ ] In edit mode: opens at the Connect step, Source step shown completed + not navigable; name field read-only; credential fields derived from `editConnection.credentialEnvVars`; Tables step shows existing tables (seeded preview); Review primary action reads "Save changes".
- [ ] Save calls `updateSavedConnectionCommand({ name, spec_text, credentials })` — `spec_text` is `null` when the textarea is empty, otherwise the pasted spec.
- [ ] `tsc` clean.

**Scope Boundary:**
- IN scope: `AddRestApiWizard.tsx` only.
- OUT of scope: `DatasourceSidebar.tsx` (task-5), `control.ts`. Do NOT change the no-`editConnection` add flow behavior.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Extend imports + props.** Add `updateSavedConnectionCommand` to the `@/daemon/control` import. Update the props type and signature:

```ts
export type AddRestApiWizardProps = {
  onClose: () => void;
  open: boolean;
  editConnection?: ConnectionTemplate | null;
};
```

```ts
export default function AddRestApiWizard({
  onClose,
  open,
  editConnection = null,
}: AddRestApiWizardProps) {
  const editMode = editConnection !== null;
```

- [ ] **Step 2: Branch the open/reset effect** so edit mode pre-fills instead of clearing. Replace the body of the `useEffect(() => { if (!open) return; ... }, [open])` to depend on `[open, editConnection]` and, when `editConnection` is set, initialize from it:

```ts
  useEffect(() => {
    if (!open) return;

    if (editConnection) {
      setStepIndex(1); // Connect
      setSourceMode("saved");
      setSelectedProvider(null);
      setSelectedSavedConnection(editConnection);
      setDatasourceName(editConnection.name);
      setCredentials({});
      setSavedConnectionCredentials({});
      setMissingSavedCredentialKeys([]);
      setSpecText("");
      setTablePreview(tablePreviewFromTemplate(editConnection));
      setConnectionOnly(editConnection.tables.length === 0);
      setProviderSearch("");
      setProviderCategory("All");
      setPendingPreview(false);
      setPendingAdd(false);
      setError(null);
      return;
    }

    // --- existing fresh-add reset (unchanged) ---
    setStepIndex(0);
    setSourceMode(null);
    setProviderSearch("");
    setProviderCategory("All");
    setSelectedProvider(null);
    setSelectedSavedConnection(null);
    setDatasourceName("");
    setCredentials({});
    setSavedConnectionCredentials({});
    setMissingSavedCredentialKeys([]);
    setSpecText("");
    setTablePreview(null);
    setConnectionOnly(false);
    setPendingPreview(false);
    setPendingAdd(false);
    setError(null);
  }, [open, editConnection]);
```

- [ ] **Step 3: Drive credential fields from the template in edit mode.** Replace the `credentialFields` memo:

```ts
  const credentialFields = useMemo(
    () =>
      editConnection
        ? editConnection.credentialEnvVars.map(
            (key): CredentialField => ({ key, label: key, type: "password" }),
          )
        : credentialFieldsForProvider(selectedProvider),
    [editConnection, selectedProvider],
  );
```

- [ ] **Step 4: Add the save handler** (next to `handleAddDatasource`):

```ts
  const handleSaveEdit = async () => {
    if (!editConnection) return;

    const trimmedSpec = specText.trim();
    const credentialPairs = credentialFields
      .map((field): [string, string] => [
        field.key,
        credentials[field.key]?.trim() ?? "",
      ])
      .filter(([, value]) => value.length > 0);

    setPendingAdd(true);
    setError(null);

    try {
      const response = await daemonControl(
        updateSavedConnectionCommand({
          name: editConnection.name,
          spec_text: trimmedSpec.length === 0 ? null : trimmedSpec,
          credentials: credentialPairs,
        }),
      );
      attachedSavedConnectionFromDaemonControlResponse(response);
      onClose();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setPendingAdd(false);
    }
  };
```

- [ ] **Step 5: Route the footer primary action.** In the footer `onClick`, handle edit mode at the review step, and label it:

```ts
                if (step === "source" && sourceMode === "saved" && !editMode) {
                  void handleAttachSavedConnection();
                } else if (step === "review") {
                  if (editMode) void handleSaveEdit();
                  else void handleAddDatasource();
                } else {
                  goNext();
                }
```

```ts
  const primaryActionLabel =
    step === "review"
      ? editMode
        ? "Save changes"
        : "Add datasource"
      : step === "source" && sourceMode === "saved"
        ? "Use connection"
        : "Continue";
```

- [ ] **Step 6: Lock Source navigation + name in edit mode.** In the stepper `<button>`, disable the Source step when editing:

```ts
                    disabled={
                      (editMode && index === 0) || (!complete && !active)
                    }
```

Pass a read-only flag into `ConnectStep` and render the name input read-only. Add `nameReadOnly` to `ConnectStep`'s props and:

```tsx
          <input
            aria-label="Datasource name"
            className="mt-1 h-9 w-full rounded border border-gray-300 bg-white px-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-indigo-600 disabled:bg-gray-50 disabled:text-gray-500"
            disabled={nameReadOnly}
            onChange={(event) => onDatasourceNameChange(event.currentTarget.value)}
            placeholder="stripe_reporting"
            value={datasourceName}
          />
```

Pass `nameReadOnly={editMode}` and the parent-computed `credentialFields` from the wizard into `ConnectStep` (so it uses the template-derived fields instead of recomputing from `selectedProvider`). Update `ConnectStep` to accept `fields` and `nameReadOnly` props rather than calling `credentialFieldsForProvider` internally.

- [ ] **Step 7: Add the preview-seeding helper** near the other helpers at the bottom of the file:

```ts
function tablePreviewFromTemplate(
  connection: ConnectionTemplate,
): OpenApiTablePreview | null {
  if (connection.tables.length === 0) return null;
  return {
    tables: connection.tables.map((table) => ({
      name: table.name,
      path: "",
      responsePath: null,
      columns: table.columns.map((column) => ({
        name: column.name,
        ty: column.sqlType,
        json: "",
      })),
    })),
  };
}
```

> If `Table`'s column field is not `sqlType`, match the actual `ConnectionTemplate["tables"][number]["columns"]` shape from `@/bindings/Table`. Verify against the binding before finalizing.

- [ ] **Step 8: Verify** — `tsc --noEmit` clean; manually reason through: opening with `editConnection` lands on Connect with name filled+disabled, credential rows from env vars, Tables shows existing tables, Review says "Save changes".

- [ ] **Step 9: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx
git commit -m "feat(jute-notebook): edit mode for AddRestApiWizard (prefill from saved connection)"
```

---

### Task 5: Edit button in the Datasource sidebar

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/DatasourceSidebar.tsx`

**Depends on:** task-4

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] Expanded `SavedConnectionRow` shows an "Edit" button beside "Delete saved connection".
- [ ] Clicking Edit opens `AddRestApiWizard` in edit mode with that connection.
- [ ] Closing the wizard clears edit state; the existing `connections://changed` listener refreshes the list after save.
- [ ] `tsc` clean.

**Scope Boundary:**
- IN scope: `DatasourceSidebar.tsx` only.
- OUT of scope: `AddRestApiWizard.tsx` (task-4) and backend. Do NOT change attach/delete behavior.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add edit state** in `DatasourceSidebar`:

```ts
  const [editingConnection, setEditingConnection] = useState<ConnectionTemplate | null>(null);
```

- [ ] **Step 2: Render the wizard with edit support.** Replace the existing `<AddRestApiWizard .../>`:

```tsx
      <AddRestApiWizard
        open={apiModalOpen || editingConnection !== null}
        editConnection={editingConnection}
        onClose={() => {
          setApiModalOpen(false);
          setEditingConnection(null);
        }}
      />
```

- [ ] **Step 3: Thread an `onEdit` callback** through `SavedConnectionsSection` → `SavedConnectionRow`. Add `onEdit: (connection: ConnectionTemplate) => void;` to both prop types, pass it in `DatasourceSidebar`:

```tsx
          <SavedConnectionsSection
            connections={savedConnections}
            expandedName={expandedSavedConnection}
            notice={savedConnectionNotice}
            onAttach={(name) => void handleAttachSavedConnection(name)}
            onDelete={(name) => void handleDeleteSavedConnection(name)}
            onEdit={(connection) => setEditingConnection(connection)}
            onToggle={(name) =>
              setExpandedSavedConnection((current) =>
                current === name ? null : name,
              )
            }
          />
```

and forward `onEdit` from `SavedConnectionsSection` to each `SavedConnectionRow`.

- [ ] **Step 4: Add the Edit button** in `SavedConnectionRow`'s expanded panel, beside the delete button:

```tsx
          <div className="flex items-center gap-3">
            <button
              aria-label={`Edit saved connection ${connection.name}`}
              className="text-xs font-medium text-gray-600 transition-colors hover:text-gray-950"
              onClick={() => onEdit(connection)}
              type="button"
            >
              Edit
            </button>
            <button
              aria-label={`Delete saved connection ${connection.name}`}
              className="text-xs font-medium text-red-600 transition-colors hover:text-red-700"
              onClick={() => onDelete(connection.name)}
              type="button"
            >
              Delete saved connection
            </button>
          </div>
```

(Replace the existing standalone delete button with this grouped pair.)

- [ ] **Step 5: Verify** — `tsc --noEmit` clean; reason through: expand a saved connection → Edit → wizard opens pre-filled; Save → wizard closes → list refreshes via `connections://changed`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/DatasourceSidebar.tsx
git commit -m "feat(jute-notebook): edit button opens AddRestApiWizard for saved connections"
```

---

## Self-Review

**Spec coverage:**
- Entry point (sidebar Edit button) → task-5. ✓
- Wizard edit mode / prefill / Source skip / name read-only / Save changes → task-4. ✓
- Spec-text accommodation (blank=keep, new spec=regenerate) → task-2 (backend) + task-4 (UI seeds preview, sends null). ✓
- New `UpdateSavedConnection` command + handler returning `AttachedSavedConnection` → task-1 + task-2. ✓
- `control.ts` builder + bindings regen → task-3. ✓
- Tests (commands.rs round-trip, e2e, control.test.ts) → task-1/2/3. ✓

**Placeholder scan:** All code steps contain concrete code. Two explicit "verify against the binding" notes (Table column field; e2e request helper) are guardrails, not placeholders — the worker confirms the exact local shape.

**Type consistency:** `UpdateSavedConnectionInput.spec_text` is `string | null` (matches the Rust `Option<String>` → TS `string | null`); builder always sends `credentials: []` default; handler returns `AttachedSavedConnection` decoded by the existing `attachedSavedConnectionFromDaemonControlResponse`, consistent across task-2/3/4.

**DAG validation:** `1→2`, `1→3→4→5`. No cycles. task-2 ∥ task-3 after task-1. Frontend chain is genuinely sequential (interface consumption).

**beads compatibility:** every task has a unique ID, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary with a `scope_drift` instruction.

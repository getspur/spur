# Notebook Datasource Schema Catalog Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Conversation-approved design delta from 2026-06-16, extending `docs/superpowers/specs/2026-06-01-api-datasource-onboarding-ui-design.md`
**Design epic:** Current brain session approval; no standalone design epic was created for this delta.

**Goal:** Make each notebook behave like one implicit datasource catalog where user-created connection schemas expose directly queryable tables as `SELECT * FROM <schema>.<table>`.

**Architecture:** Keep the notebook catalog implicit and spend the two visible SQL parts on schema and table. `DatasourceEntry.name` becomes the schema/connection namespace, provider remains metadata for UI grouping, and the datasource setup cell creates schema-qualified views over the backing local files, attached databases, and REST/RSS table functions. API tags and product areas stay as UI grouping or table-name prefixes only when needed to avoid collisions.

**Tech Stack:** Rust (`spur-notebook`, `rest-table-gateway`, Tauri command bindings), DuckDB SQL setup cells, React + TypeScript notebook UI, Vitest, existing `scripts/spur-cargo` / `scripts/spur-pnpm` wrappers.

---

## File Structure Mapping

- `crates/spur-notebook/src/mcp/mod.rs` — datasource setup SQL generation, API table registration, backend tests for `schema.table` exposure.
- `crates/spur-notebook/rest-table-gateway/src/adapter/openapi.rs` — OpenAPI table naming, tag-aware collision fallback, manifest TOML output tests.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.tsx` — query cell generation and table display using `<schema>.<table>`.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx` — schema-name language, review output, and attachable-only category behavior.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx` and `DatasourcePanel.test.tsx` — UI flow assertions.
- `crates/spur-notebook/tests/api_datasource_import_e2e.rs` or existing notebook datasource tests — final integration coverage.

## Invariants

- The notebook catalog is implicit. Users type two-part names: `<schema>.<table>`.
- Schema names are user-controlled datasource/connection names. For API providers, default schema names should be connection instances such as `github_work`, not provider names such as `github`.
- Provider is metadata and UI grouping only.
- OpenAPI tags/product areas do not create another SQL level. They may prefix table names only when needed for disambiguation.
- No attachable wizard path may end in a SQL recipe/handoff. If a source cannot attach and become directly queryable, the wizard must not present it as attachable.
- No-tag/no-namespace API fallback is frictionless: tables live directly under the selected schema, e.g. `github_work.repositories`.

---

## Task 1: Backend Schema-Qualified Datasource Setup

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/mod.rs`
- Test: existing `#[cfg(test)]` module in `crates/spur-notebook/src/mcp/mod.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] API datasource setup emits `CREATE SCHEMA IF NOT EXISTS <schema>` plus `CREATE OR REPLACE VIEW <schema>.<table> AS SELECT * FROM <backing_table_function>()` for each exposed table.
- [ ] Local single-table datasources expose a direct two-part relation using the datasource name as schema and `main` as the default table: `SELECT * FROM <schema>.main`.
- [ ] DuckDB and SQLite multi-table datasources expose each discovered table as `SELECT * FROM <schema>.<table>` while preserving any existing raw attach behavior needed internally.
- [ ] Existing flat datasource behavior remains backward compatible where practical, but new query-cell generation will use the schema-qualified convention.
- [ ] Backend unit tests cover API, local file, and attached database setup statement generation.
- [ ] Verification command passes: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook datasource_setup -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: setup SQL helpers, `datasource_setup_statements`, `register_api_datasource_entry_inner`, tests in `mod.rs`.
- OUT of scope: OpenAPI name extraction, React UI, generated TypeScript bindings.
- If a change requires frontend files or rest-table-gateway files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write failing backend tests** in `crates/spur-notebook/src/mcp/mod.rs`.

```rust
#[test]
fn api_datasource_setup_creates_schema_qualified_views() {
    let entry = jute::commands::DatasourceEntry {
        name: "github_work".to_string(),
        path: "github".to_string(),
        kind: jute::commands::DatasourceKind::ApiTables,
        group: Some("API".to_string()),
        columns: Vec::new(),
        row_count: None,
        tables: vec![jute::commands::Table {
            name: "repositories".to_string(),
            columns: Vec::new(),
            row_count: None,
        }],
    };

    let statements = datasource_setup_statements(&entry);
    assert!(statements.iter().any(|sql| {
        sql == "CREATE SCHEMA IF NOT EXISTS github_work"
    }));
    assert!(statements.iter().any(|sql| {
        sql == "CREATE OR REPLACE VIEW github_work.repositories AS SELECT * FROM github_repositories()"
    }));
}

#[test]
fn file_datasource_setup_exposes_main_table_under_schema() {
    let entry = jute::commands::DatasourceEntry {
        name: "orders_file".to_string(),
        path: "/tmp/orders.parquet".to_string(),
        kind: jute::commands::DatasourceKind::Parquet,
        group: None,
        columns: Vec::new(),
        row_count: None,
        tables: Vec::new(),
    };

    let statements = datasource_setup_statements(&entry);
    assert!(statements.iter().any(|sql| {
        sql == "CREATE SCHEMA IF NOT EXISTS orders_file"
    }));
    assert!(statements.iter().any(|sql| {
        sql == "CREATE OR REPLACE VIEW orders_file.main AS SELECT * FROM read_parquet('/tmp/orders.parquet')"
    }));
}
```

- [ ] **Step 2: Run test to verify failure.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook datasource_setup -- --nocapture`

Expected: FAIL because API datasource setup currently returns no SQL statements and file setup creates only flat views.

- [ ] **Step 3: Implement setup helpers.**

Add focused helpers near the existing SQL helpers:

```rust
#[cfg(feature = "datasource-introspect")]
fn datasource_schema_name(entry: &jute::commands::DatasourceEntry) -> &str {
    entry.name.as_str()
}

#[cfg(feature = "datasource-introspect")]
fn api_backing_table_function(entry: &jute::commands::DatasourceEntry, table_name: &str) -> String {
    format!("{}_{}", entry.path, table_name)
}

#[cfg(feature = "datasource-introspect")]
fn create_schema_statement(schema: &str) -> String {
    format!("CREATE SCHEMA IF NOT EXISTS {}", sql_identifier(schema))
}

#[cfg(feature = "datasource-introspect")]
fn create_schema_view_statement(schema: &str, table: &str, select_from: String) -> String {
    format!(
        "CREATE OR REPLACE VIEW {}.{} AS SELECT * FROM {}",
        sql_identifier(schema),
        sql_identifier(table),
        select_from,
    )
}
```

Then update `datasource_setup_statements` so every attached datasource emits a schema and schema-qualified views. Use `main` as the table name for single-object file sources. For DuckDB/SQLite, attach the raw database with an internal alias if needed, then create wrapper views under the user-facing schema.

- [ ] **Step 4: Preserve safety and compatibility.**

Keep existing flat aliases only if they do not conflict with creating schema-qualified views. If flat aliases are kept, add a comment that they are backward-compatible legacy aliases and not the canonical query convention.

- [ ] **Step 5: Verify and commit.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook datasource_setup -- --nocapture`

Commit:

```bash
git add crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): DS1 expose datasource schema views"
```

---

## Task 2: OpenAPI and API Table Names Without Provider Flattening

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/openapi.rs`
- Modify: `crates/spur-notebook/src/mcp/mod.rs`
- Test: existing tests in those files

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] `api_datasource_table` exposes display table names without prefixing the adapter/source name.
- [ ] Backing API table function names remain derivable from `entry.path` plus table name for Task 1's setup views.
- [ ] OpenAPI table naming still prefers `operationId`, then a sanitized last concrete path segment, then `table`.
- [ ] OpenAPI tags/product areas are not emitted as SQL schemas.
- [ ] When two generated OpenAPI table names collide, use the first sanitized tag as a table-name prefix when present; otherwise append a deterministic numeric suffix.
- [ ] Untagged APIs with no namespace remain frictionless: `connection_schema.repositories`, not `connection_schema.api_repositories` or `connection_schema.main.repositories`.
- [ ] Verification commands pass:
  - `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook openapi -- --nocapture`
  - `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook api_datasource -- --nocapture`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: OpenAPI table name generation and API table display/backing name separation.
- OUT of scope: wizard layout, sidebar query generation, non-OpenAPI source-category behavior.
- If this requires changing generated TypeScript bindings, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing OpenAPI naming tests.**

Add tests in `openapi.rs` using small inline specs:

```rust
#[test]
fn untagged_openapi_table_uses_operation_or_path_without_schema_prefix() {
    let spec = parse_spec(r#"
openapi: 3.0.0
info: { title: Demo, version: "1" }
paths:
  /repositories:
    get:
      operationId: listRepositories
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: array
                items:
                  type: object
                  properties:
                    id: { type: string }
"#).unwrap();

    let tables = spec_to_tables(&spec);
    assert_eq!(tables[0].name, "listrepositories");
}

#[test]
fn openapi_table_name_collision_uses_tag_prefix_or_suffix() {
    let spec = parse_spec(r#"
openapi: 3.0.0
info: { title: Demo, version: "1" }
paths:
  /billing/items:
    get:
      tags: [Billing]
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: array
                items: { type: object, properties: { id: { type: string } } }
  /crm/items:
    get:
      tags: [CRM]
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: array
                items: { type: object, properties: { id: { type: string } } }
"#).unwrap();

    let names = spec_to_tables(&spec)
        .into_iter()
        .map(|table| table.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"billing_items".to_string()));
    assert!(names.contains(&"crm_items".to_string()));
}
```

- [ ] **Step 2: Add failing API display-name test** in `mod.rs`.

```rust
#[test]
fn api_datasource_table_keeps_display_name_unprefixed() {
    let table_def = spur_rest_table_gateway::adapter::TableDef {
        name: "repositories".to_string(),
        schema: Arc::new(arrow_schema::Schema::empty()),
        kind: spur_rest_table_gateway::adapter::TableKind::Table,
    };
    let table = api_datasource_table("github", table_def);
    assert_eq!(table.name, "repositories");
}
```

Adjust imports/types to match the local module style.

- [ ] **Step 3: Run tests to verify failure.**

Run both verification commands from the acceptance criteria.

- [ ] **Step 4: Implement deterministic naming.**

Refactor OpenAPI naming into two phases:

1. Generate a base name from `operationId` or path.
2. Resolve collisions across the full table list by using first tag prefix when available, then `_2`, `_3`, etc.

Do not add `schema` to `TableCfg`.

- [ ] **Step 5: Verify and commit.**

Run both verification commands.

Commit:

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/openapi.rs crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): DS2 keep API tables schema local"
```

---

## Task 3: Sidebar Query Generation Uses `<schema>.<table>`

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.test.tsx`

**Depends on:** `task-1`, `task-2`

**Acceptance Criteria:**
- [ ] Query cells created from datasource table actions use `SELECT * FROM <schema>.<table> LIMIT 100;`.
- [ ] The schema is the parent `DatasourceEntry.name`, not the provider.
- [ ] The table is `Table.name` and is not adapter-prefixed.
- [ ] UI labels in the datasource panel show the queryable relation as `<schema>.<table>`.
- [ ] Existing single-file datasource display remains readable and uses `<schema>.main` where a concrete table object is needed.
- [ ] Verification command passes: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/DatasourcePanel.test.tsx`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: datasource panel rendering, query-cell source helper, tests for generated SQL text.
- OUT of scope: wizard source-family selection and backend SQL generation.
- If backend types must change, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing panel tests.**

Add or update tests so scheduling an API table named `repositories` under datasource entry `github_work` inserts:

```sql
SELECT * FROM github_work.repositories LIMIT 100;
```

The assertion should fail against the current `SELECT * FROM github_repositories() LIMIT 100;` behavior.

- [ ] **Step 2: Run the focused test to verify failure.**

Run: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/DatasourcePanel.test.tsx`

- [ ] **Step 3: Update query generation.**

Change the helper from table-only:

```ts
function tableFunctionQuerySource(tableName: string): string
```

to relation-aware:

```ts
function tableRelationQuerySource(schemaName: string, tableName: string): string {
  return `SELECT * FROM ${duckDbIdentifier(schemaName)}.${duckDbIdentifier(tableName)} LIMIT 100;\n`;
}
```

Thread the parent datasource name into the schedule-table action instead of passing only `table.name`.

- [ ] **Step 4: Update labels.**

Where table rows or quick actions display table names, show `schema.table` for multi-table/API datasource tables. Keep compact styling; do not add explanatory copy.

- [ ] **Step 5: Verify and commit.**

Run: `scripts/spur-pnpm test -- src/ui/notebook/sidebar/DatasourcePanel.test.tsx`

Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.test.tsx
git commit -m "feat(spur-notebook): DS3 query datasource schema tables"
```

---

## Task 4: Wizard Shows Attachable Schemas, Not SQL Handoffs

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/datasourceWizardModel.ts`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/datasourceWizardModel.test.ts`

**Depends on:** `task-2`, `task-3`

**Acceptance Criteria:**
- [ ] REST/API and RSS review screens show queryable objects as `<schema>.<table>`.
- [ ] The user-facing name field is labeled as a schema/connection name, while preserving command payload compatibility with the current `name` field.
- [ ] REST manual/connection-only flow still attaches a schema, even when no tables are imported yet.
- [ ] Non-attachable generic families (`cloud_object_storage`, `lakehouse`, `database`, `advanced_sql`) are not presented as successful attach flows. They are either hidden from the primary chooser or disabled with a non-actionable status and cannot reach the footer CTA.
- [ ] Local file remains attachable through the existing picker/callback and review copy no longer frames it as a SQL recipe.
- [ ] Verification commands pass:
  - `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx`
  - `scripts/spur-pnpm test -- src/ui/notebook/datasourceWizardModel.test.ts`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: wizard labels, category gating, review rows, tests.
- OUT of scope: backend attach semantics, sidebar query generation, rest-table-gateway OpenAPI parsing.
- If this task needs Rust files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing UI tests.**

Add assertions for:

```ts
expect(screen.getByLabelText("Schema name")).toBeInTheDocument();
expect(screen.getByText("github_work.repositories")).toBeInTheDocument();
```

Add a test that non-attachable categories cannot progress to an attach CTA:

```ts
await user.click(screen.getByRole("button", { name: /URL or object storage/i }));
expect(screen.getByRole("button", { name: /Continue/i })).toBeDisabled();
```

Use exact labels that match the final UI text.

- [ ] **Step 2: Run tests to verify failure.**

Run both verification commands from the acceptance criteria.

- [ ] **Step 3: Update source-family model.**

Add explicit attachability metadata:

```ts
attachable: true | false;
```

Set `file`, `rest_api`, and `rss` to `true`. Set the remaining families to `false` until backend attach contracts exist. Keep their metadata available for future work, but do not let the wizard route them to a fake attach flow.

- [ ] **Step 4: Update wizard copy and review rows.**

Use "Schema name" for the name input. In review, render stable query targets from the selected schema and table names:

```ts
const relation = `${datasourceName.trim()}.${table.name}`;
```

For REST connection-only/manual with no tables, show the schema as attachable and clearly indicate that no tables are currently exposed. Do not show copy/paste SQL snippets as the final result.

- [ ] **Step 5: Verify and commit.**

Run both verification commands.

Commit:

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/datasourceWizardModel.ts crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/datasourceWizardModel.test.ts
git commit -m "feat(spur-notebook): DS4 align wizard attachable schemas"
```

---

## Task 5: End-to-End Verification for Schema-Qualified Datasources

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/tests/api_datasource_import_e2e.rs`
- Modify: relevant existing notebook datasource tests if this flow already has better coverage

**Depends on:** `task-1`, `task-2`, `task-3`, `task-4`

**Acceptance Criteria:**
- [ ] E2E coverage proves a REST/OpenAPI datasource attaches under a user schema and can be queried as `<schema>.<table>`.
- [ ] E2E coverage proves RSS attaches under a user schema and exposes table names without provider/source prefixes.
- [ ] Existing saved-connection attach coverage still passes with schema-qualified table names.
- [ ] Full focused backend and frontend verification commands pass:
  - `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook api_datasource -- --nocapture`
  - `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx src/ui/notebook/sidebar/DatasourcePanel.test.tsx`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: focused e2e and integration tests for the completed convention.
- OUT of scope: broad UI redesign, new datasource category backends, OAuth/secrets persistence.
- If the final integrated convention fails because an upstream task made a conflicting assumption, emit `risk` with concrete failing test output.

**Implementation:**
- [ ] **Step 1: Add failing integration assertions.**

In the existing API datasource e2e path, after attaching a source named `github_work` or equivalent, assert that the catalog/setup exposes a relation matching:

```sql
SELECT * FROM github_work.repositories LIMIT 1
```

Use the local test harness patterns already present in `api_datasource_import_e2e.rs`; do not introduce a network dependency.

- [ ] **Step 2: Add RSS/saved-connection assertions.**

Assert RSS tables are displayed/queryable under the user schema, for example:

```sql
SELECT * FROM rss_news.entries LIMIT 1
```

Use the existing RSS catalog tests as the source of expected table names.

- [ ] **Step 3: Run tests to verify failures if upstream work is absent.**

Run the two verification commands from the acceptance criteria.

- [ ] **Step 4: Adjust tests to the final integrated behavior.**

Keep tests focused on externally visible behavior: attached datasource schemas, table names, and generated query source. Do not assert incidental implementation details such as exact setup-cell statement order unless required for correctness.

- [ ] **Step 5: Verify and commit.**

Run the two verification commands.

Commit:

```bash
git add crates/spur-notebook/tests/api_datasource_import_e2e.rs crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/DatasourcePanel.test.tsx
git commit -m "test(spur-notebook): DS5 cover schema qualified datasources"
```

---

## Dependency DAG

```text
task-1
  -> task-2
       -> task-3
       -> task-4
task-1, task-2, task-3, task-4
  -> task-5
```

`task-3` and `task-4` can run in parallel after `task-2` completes. `task-5` is the final integration gate.

## Plan Self-Review

- **Spec coverage:** The plan implements the approved implicit notebook catalog, user/connection schema names, directly queryable `<schema>.<table>` convention, no-friction API fallback, and attachable-only wizard paths.
- **Placeholder scan:** The plan contains no placeholder fields, open-ended tasks, or deferred implementation sections.
- **Type consistency:** Tasks use existing `DatasourceEntry.name` as schema and existing `Table.name` as table. No new bridge type is required unless a worker discovers a concrete blocker and emits `scope_drift`.
- **DAG validation:** The dependency graph is acyclic. Backend setup precedes naming, UI query generation and wizard copy can run in parallel after naming, and integration tests run last.
- **beads compatibility:** Every task has a unique task ID, explicit dependencies, acceptance criteria, suggested worker, and a scope boundary.

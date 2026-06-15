# DuckDB SQL Cell Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-15-duckdb-sql-cell-design.md`
**Design epic:** `bd-2rgcu`
**Plan id:** `sql-cell-duckdb-2026-06-15`

**Goal:** Add a first-class DuckDB SQL cell (`code_type = "sql"`) that runs SQL in the shared in-kernel DuckDB connection, with native chip/switcher/highlighting and a result port.

**Architecture:** Approach A+1. SQL is a new `code_type`, not a new kernel. It routes to the `python3` kernelspec; the cell source transpiles to a `duckdb` call on the process-global default connection (the same connection `datasource_setup_source` already uses), giving cross-SQL-cell state reuse and Python DataFrame interop for free. The result binds to a kernel global named by the cell's produced port, so the existing produced-port capture publishes it as Arrow.

**Tech Stack:** Rust (`jute` src-tauri crate, `spur-notebook` core crate, ts-rs), TypeScript/React (jute-notebook frontend, CodeMirror `@codemirror/lang-sql`, vitest), DuckDB (Python module-level connection).

**Build commands (MANDATORY):** `scripts/spur-cargo` (never bare cargo), `scripts/spur-pnpm` (never bare pnpm). Force-remote from the sandbox with `SPUR_REMOTE=1`.

---

## Dependency DAG

```
BT1 (root: CodeType::Sql + binding)
 ├─ BT2 (cell_runner routing + transpile) ─ BT3 (SQL lineage extraction)
 ├─ FT1 (cellLanguage.ts token + menu)    ─ FT3 (NotebookCells SQL chrome)
 └─ FT2 (CellInput SQL highlighting)
```

BT1 is the only root. After BT1, the backend chain (BT2→BT3) and the three frontend
tasks run in parallel; FT3 waits on FT1.

---

### Task BT1: Add `CodeType::Sql` and regenerate the binding

**Task ID:** `bt1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs:291-321` (`CodeType` enum, `kernelspec_for`, `code_type_for_spec`)
- Regenerate: `crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `CodeType::Sql` exists; serde renames it to `"sql"` (round-trips).
- [ ] `kernelspec_for(CodeType::Sql) == "python3"`.
- [ ] `code_type_for_spec` is unchanged (SQL is NOT inferable from `python3`; it is carried explicitly by `code_type` metadata). Add a code comment stating this.
- [ ] `CodeType.ts` regenerated and now includes `"sql"`.
- [ ] Tests pass; no compilation errors.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the `CodeType` enum + `kernelspec_for` in `backend/notebook.rs`; the regenerated binding file.
- OUT of scope: `cell_runner.rs`, any frontend `.tsx`, datasource code. If you need to touch them, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `backend/notebook.rs`, or add one if absent):

```rust
#[test]
fn sql_code_type_routes_to_python3_and_serdes_lowercase() {
    assert_eq!(kernelspec_for(CodeType::Sql), "python3");
    assert_eq!(serde_json::to_string(&CodeType::Sql).unwrap(), "\"sql\"");
    assert_eq!(
        serde_json::from_str::<CodeType>("\"sql\"").unwrap(),
        CodeType::Sql
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute sql_code_type_routes`
Expected: FAIL (`no variant named Sql`).

- [ ] **Step 3: Write minimal implementation**

Add the variant to the enum:

```rust
    /// Go code routed to the `gonb` kernelspec.
    Go,
    /// DuckDB SQL transpiled to a `duckdb` call on the shared `python3` kernel.
    Sql,
}
```

Add the arm to `kernelspec_for` (SQL shares the Python kernel):

```rust
        CodeType::Go => "gonb",
        CodeType::Sql => "python3",
    }
```

Leave `code_type_for_spec` as-is and add a comment above its `"python3"` arm:

```rust
    // NOTE: "python3" maps back to Python only. A SQL cell also runs on python3
    // but is distinguished solely by its explicit `code_type` metadata.
    match spec_name {
        "python3" => Some(CodeType::Python),
```

- [ ] **Step 4: Run test to verify it passes, then regenerate the binding**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute sql_code_type_routes`
Expected: PASS.

Regenerate the ts-rs binding (the `#[ts(export)]` export runs as part of the crate's test/export step):

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p jute export_bindings`
Then confirm `crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts` now reads:

```ts
export type CodeType = "python" | "javascript" | "rust" | "go" | "sql";
```

(If the repo uses a different export entrypoint, run the crate's full test suite once; ts-rs writes the binding on export. Do not hand-edit the generated file beyond confirming the result.)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs \
        crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts
git commit -m "feat(spur-notebook): sql-cell-duckdb add CodeType::Sql + binding"
```

---

### Task BT2: Route and transpile SQL cells in the DAG runner

**Task ID:** `bt2`

**Files:**
- Modify: `crates/spur-notebook/src/dag/cell_runner.rs:191-200` (`code_type_kernelspec`) and the cell-source preparation path that dispatches code to the kernel.

**Depends on:** `bt1`

**Acceptance Criteria:**
- [ ] `code_type_kernelspec("sql")` returns `Some("python3")`.
- [ ] A new `transpile_sql_cell(sql: &str, produced: Option<&str>) -> String` wraps SQL into Python that runs on the shared default DuckDB connection and binds the produced relation to a kernel global (Arrow), or runs anonymously when `produced` is `None`.
- [ ] The runner calls `transpile_sql_cell` for cells whose `code_type == "sql"` before dispatch, passing `CellRouting.produced`.
- [ ] Tests pass; no compilation errors.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `cell_runner.rs` routing + transpile + its call site.
- OUT of scope: `backend/notebook.rs`, frontend, lineage extraction (that is `bt3`). If the transpile needs referenced-relation parsing, STOP at binding the produced global and leave dependency extraction to `bt3`; do not inline it here.

**Implementation:**
- [ ] **Step 1: Write the failing tests** (in the `cell_runner.rs` test module):

```rust
#[test]
fn code_type_kernelspec_maps_sql_to_python3() {
    assert_eq!(code_type_kernelspec("sql").as_deref(), Some("python3"));
}

#[test]
fn transpile_sql_binds_produced_relation_as_arrow() {
    let py = transpile_sql_cell("SELECT 1 AS x", Some("answer"));
    assert!(py.contains("import duckdb"));
    assert!(py.contains("answer ="));
    assert!(py.contains(".arrow()"));
    assert!(py.contains("SELECT 1 AS x"));
}

#[test]
fn transpile_sql_runs_anonymously_without_produced() {
    let py = transpile_sql_cell("SELECT 1", None);
    assert!(py.contains("duckdb.sql("));
    assert!(!py.contains(" = duckdb.sql"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook transpile_sql`
Expected: FAIL (`transpile_sql_cell` not found).

- [ ] **Step 3: Implement**

Add the `"sql"` arm to `code_type_kernelspec` (SQL shares the Python kernel):

```rust
        "go" => Some(jute_kernelspec_for(CodeType::Go).to_owned()),
        "sql" => Some(jute_kernelspec_for(CodeType::Sql).to_owned()),
        "spur" => Some("spur".to_owned()),
```

Add the transpile helper. It runs on DuckDB's module-level default connection
(process-global, so views/temp tables persist across SQL cells, datasource views are
visible, and other kernel-global DataFrames resolve via replacement scans):

```rust
/// Wrap a DuckDB SQL cell into Python executed on the shared default connection.
/// When `produced` is set, the Arrow result binds to that kernel global so the
/// existing produced-port capture publishes it (and downstream cells can read it
/// by name). Otherwise the query runs for its side effects / preview only.
fn transpile_sql_cell(sql: &str, produced: Option<&str>) -> String {
    // Triple-quoted raw Python string; escape any embedded triple quotes.
    let literal = sql.replace("\"\"\"", "\\\"\\\"\\\"");
    match produced {
        Some(name) => format!(
            "import duckdb\n{name} = duckdb.sql(r\"\"\"{literal}\"\"\").arrow()\n{name}"
        ),
        None => format!("import duckdb\nduckdb.sql(r\"\"\"{literal}\"\"\")"),
    }
}
```

Call it in the source-prep path. Where the runner currently takes the cell's code and
sends it to the kernel, branch on the cell's `code_type`: when it is `"sql"`, replace
the dispatched source with `transpile_sql_cell(&original_source, routing.produced.as_deref())`.
Read `code_type` from `cell.metadata.spur.code_type` (the same place `resolve_cell_routing`
already reads it). Keep all other code types unchanged.

- [ ] **Step 4: Run to verify pass**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook transpile_sql code_type_kernelspec_maps_sql`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/cell_runner.rs
git commit -m "feat(spur-notebook): sql-cell-duckdb route + transpile SQL cells"
```

---

### Task BT3: Auto-extract SQL relation dependencies for lineage

**Task ID:** `bt3`

**Files:**
- Modify: `crates/spur-notebook/src/dag/cell_runner.rs` (`resolve_cell_routing` + a new `sql_referenced_relations` helper)

**Depends on:** `bt2`

**Acceptance Criteria:**
- [ ] `sql_referenced_relations(sql: &str) -> Vec<String>` returns the identifiers following `FROM` / `JOIN` (case-insensitive), de-duplicated, excluding subquery aliases.
- [ ] When the cell's `code_type == "sql"`, `resolve_cell_routing` merges these into `CellRouting.consumed` (union with any explicit `dag.consumes`), so cascade ordering and lineage track SQL deps without manual metadata.
- [ ] Tests pass; no compilation errors.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `sql_referenced_relations` + its use inside `resolve_cell_routing`.
- OUT of scope: the transpile (done in `bt2`), frontend, the produced-port path. A full DuckDB-AST parser is out of scope for v1; use the conservative tokenizer below. If you find the tokenizer cannot disambiguate a real case, emit `risk` rather than pulling in a SQL-parser dependency.

**Implementation:**
- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn sql_referenced_relations_extracts_from_and_join() {
    let rels = sql_referenced_relations(
        "SELECT * FROM matches m JOIN ds.events e USING (id) WHERE e.type = 'Goal'",
    );
    assert!(rels.iter().any(|r| r == "matches"));
    assert!(rels.iter().any(|r| r == "ds.events"));
    assert_eq!(rels.iter().filter(|r| *r == "matches").count(), 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook sql_referenced_relations`
Expected: FAIL (not found).

- [ ] **Step 3: Implement the tokenizer**

```rust
/// Conservative lineage helper: the identifier token immediately after a FROM or
/// JOIN keyword (case-insensitive). Dotted names (`ds.events`) are kept whole.
/// Not a full SQL parser; good enough to wire DAG dependency edges for v1.
fn sql_referenced_relations(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut expect_relation = false;
    for token in sql.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')') {
        let word = token.trim();
        if word.is_empty() {
            continue;
        }
        if expect_relation {
            let name = word.trim_end_matches(';');
            let is_ident = name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
            if is_ident && !name.is_empty() && !out.iter().any(|r| r == name) {
                out.push(name.to_owned());
            }
            expect_relation = false;
            continue;
        }
        let upper = word.to_ascii_uppercase();
        if upper == "FROM" || upper == "JOIN" {
            expect_relation = true;
        }
    }
    out
}
```

In `resolve_cell_routing`, after computing `consumed`, union the SQL-derived relations
when the cell is a SQL cell:

```rust
    let mut consumed: Vec<String> = dag
        .get("consumes")
        /* ...existing collect... */
        .collect();
    let is_sql = spur.get("code_type").and_then(Value::as_str) == Some("sql");
    if is_sql {
        let code = cell.get("source").and_then(Value::as_str).unwrap_or_default();
        for rel in sql_referenced_relations(code) {
            if !consumed.iter().any(|c| *c == rel) {
                consumed.push(rel);
            }
        }
    }
```

(If the cell `source` is stored as an array of lines rather than a string, join it
first; match the shape `resolve_cell_routing` already sees.)

- [ ] **Step 4: Run to verify pass**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook sql_referenced_relations`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/cell_runner.rs
git commit -m "feat(spur-notebook): sql-cell-duckdb auto-extract SQL lineage deps"
```

---

### Task FT1: Add the `sql` language token (and switcher entry)

**Task ID:** `ft1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.ts`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.test.ts` (create if absent)

**Depends on:** `bt1` (regenerated `CodeType.ts` must already include `"sql"`)

**Acceptance Criteria:**
- [ ] `CELL_LANGUAGE_TOKENS.sql` exists: label `DuckDB`, glyph `SQL`, kernelspec `python3`, accent `#F6BD3B`, warm-but-distinct chip colors.
- [ ] `CODE_LANGUAGE_ORDER` includes `"sql"` positioned after `"go"` and before `"spur"`.
- [ ] `CellLanguageMenu` renders and enables the DuckDB SQL entry with no menu-code change (it maps over `CODE_LANGUAGE_ORDER` and only disables `"spur"`).
- [ ] Tests pass; `scripts/spur-pnpm run typecheck` is clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `cellLanguage.ts` token + order, and its test.
- OUT of scope: `CellLanguageMenu.tsx` (no change needed), `CellInput.tsx`, `NotebookCells.tsx`. If a type error forces a `CellLanguageMenu.tsx` edit, the binding from `bt1` is missing `"sql"` — emit `risk` rather than editing the menu.

**Implementation:**
- [ ] **Step 1: Write the failing test** (`cellLanguage.test.ts`):

```ts
import { describe, expect, test } from "vitest";
import { CELL_LANGUAGE_TOKENS, CODE_LANGUAGE_ORDER } from "./cellLanguage";

describe("sql language token", () => {
  test("exists with DuckDB identity on the shared python3 kernel", () => {
    const t = CELL_LANGUAGE_TOKENS.sql;
    expect(t).toBeDefined();
    expect(t.label).toBe("DuckDB");
    expect(t.glyph).toBe("SQL");
    expect(t.kernelspec).toBe("python3");
    expect(t.accent.toUpperCase()).toBe("#F6BD3B");
  });
  test("ordered after go and before spur", () => {
    const i = CODE_LANGUAGE_ORDER.indexOf("sql");
    expect(i).toBeGreaterThan(CODE_LANGUAGE_ORDER.indexOf("go"));
    expect(i).toBeLessThan(CODE_LANGUAGE_ORDER.indexOf("spur"));
  });
});
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-pnpm test -- src/ui/notebook/cellLanguage.test.ts`
Expected: FAIL (`CELL_LANGUAGE_TOKENS.sql` is undefined).

- [ ] **Step 3: Implement** — add the token row after `go` and before `spur`:

```ts
  go:         { id: "go",         label: "Go",         glyph: "Go", kernelspec: "gonb",    accent: "#00ADD8", chipText: "#0a7e9e", chipBg: "#ffffff", chipBorder: "#a8deec", glyphBg: "#e5f6fb" },
  sql:        { id: "sql",        label: "DuckDB",     glyph: "SQL", kernelspec: "python3", accent: "#F6BD3B", chipText: "#8a5a00", chipBg: "#ffffff", chipBorder: "#f0d79a", glyphBg: "#fef6e0" },
  spur:       { id: "spur",       label: "AI Agent",   glyph: "✦",  kernelspec: "spur",    accent: "#7C3AED", chipText: "#6d28d9", chipBg: "#f5f3ff", chipBorder: "#ddd6fe", glyphBg: "#ffffff" },
```

And update the order:

```ts
export const CODE_LANGUAGE_ORDER: CellLanguageId[] = ["python", "javascript", "rust", "go", "sql", "spur"];
```

- [ ] **Step 4: Run to verify pass + typecheck**

Run: `scripts/spur-pnpm test -- src/ui/notebook/cellLanguage.test.ts`
Run: `scripts/spur-pnpm run typecheck`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.ts \
        crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.test.ts
git commit -m "feat(spur-notebook): sql-cell-duckdb add DuckDB language token"
```

---

### Task FT2: SQL syntax highlighting in CellInput

**Task ID:** `ft2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/CellInput.tsx:88-106` (`extensionForLanguage`)
- Modify: `crates/spur-notebook/jute-notebook/package.json` (add `@codemirror/lang-sql`)

**Depends on:** `bt1` (the `"sql"` member of `CodeType` is needed for the switch type)

**Acceptance Criteria:**
- [ ] `@codemirror/lang-sql` is a dependency.
- [ ] `extensionForLanguage("code", "sql")` returns the SQL CodeMirror extension.
- [ ] `extensionForLanguage` is `export`ed so it is unit-testable.
- [ ] Tests pass; `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `CellInput.tsx` highlighting switch + `export`, `package.json` dep.
- OUT of scope: `cellLanguage.ts`, `NotebookCells.tsx`, store changes. Do not change the CodeMirror compartment wiring beyond adding the `sql` case.

**Implementation:**
- [ ] **Step 1: Write the failing test** (`CellInput.lang.test.ts` next to the component):

```ts
import { expect, test } from "vitest";
import { extensionForLanguage } from "./CellInput";

test("sql code cells get a non-empty SQL extension", () => {
  const ext = extensionForLanguage("code", "sql");
  expect(ext).toBeTruthy();
});
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-pnpm test -- src/ui/notebook/CellInput.lang.test.ts`
Expected: FAIL (`extensionForLanguage` is not exported / `lang-sql` missing).

- [ ] **Step 3: Implement**

Add the dependency to `package.json` `dependencies` (match the installed CodeMirror major; align with `@codemirror/lang-python`'s version line):

```json
    "@codemirror/lang-sql": "^6.7.0",
```

In `CellInput.tsx`, add the import near the other language imports:

```ts
import { sql } from "@codemirror/lang-sql";
```

`export` the helper and add the `sql` case:

```ts
export function extensionForLanguage(type: CellType, codeType?: CodeType): Extension {
  if (type === "markdown") {
    return [markdown(), EditorView.lineWrapping];
  } else if (type !== "code") {
    throw new Error(`Unsupported cell type: ${type}`);
  }

  switch (codeType) {
    case "javascript":
      return javascript({ typescript: true });
    case "rust":
      return rust();
    case "go":
      return StreamLanguage.define(go);
    case "sql":
      return sql();
    case "python":
    default:
      return python();
  }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/CellInput.lang.test.ts`
Run: `scripts/spur-pnpm run typecheck`
Expected: PASS, clean. (The frontend test VM resolves the new dep through the shared store.)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/CellInput.tsx \
        crates/spur-notebook/jute-notebook/package.json \
        crates/spur-notebook/jute-notebook/src/ui/notebook/CellInput.lang.test.ts
git commit -m "feat(spur-notebook): sql-cell-duckdb highlight SQL cells"
```

---

### Task FT3: SQL cell chrome — session pill + relation-name affordance

**Task ID:** `ft3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.sql.test.tsx` (create)

**Depends on:** `ft1`

**Acceptance Criteria:**
- [ ] When a code cell's language id is `"sql"`, its header renders a `⛁ kernel session` pill and an inline-editable `→ relation` output-name input.
- [ ] Editing the relation name writes the produced port into `cell.metadata.spur.dag.produces` (a `PortSpec` with `repr: "arrow"`); clearing it removes the produced port (anonymous preview).
- [ ] The SQL result re-uses the existing produced-port preview rendering (no new grid component); SQL cells opt into the same port preview Python/produced cells use.
- [ ] Non-SQL cells are visually unchanged.
- [ ] Tests pass; `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: SQL-only header chrome in `NotebookCells.tsx` (session pill + relation-name input) and wiring the name to `dag.produces` via the existing store mutator.
- OUT of scope: `cellLanguage.ts`, `CellInput.tsx`, backend, the accent bar / gutter (already provided by the polyglot token system). Reuse the existing produced-port preview; do NOT build a new result grid. If wiring the produced port needs a store API that does not exist, emit `scope_drift` (do not add store methods here).

**Scope Drift Checkpoint:**
- If you need to touch the store (`notebook.ts`) to persist `dag.produces`, STOP and emit `scope_drift` — that is a separate task.
- If the produced-port preview component does not already exist to reuse, emit `risk`.

**Implementation:**
- [ ] **Step 1: Write the failing test** (`NotebookCells.sql.test.tsx`) — model the render harness on the existing `NotebookCells.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { SqlCellHeader } from "./NotebookCells";

test("sql cell shows the kernel-session pill and a relation-name input", () => {
  render(
    <SqlCellHeader
      relation="top_scorers"
      onRelationChange={() => {}}
    />,
  );
  expect(screen.getByText(/kernel session/i)).toBeInTheDocument();
  expect(screen.getByLabelText(/relation/i)).toHaveValue("top_scorers");
});
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.sql.test.tsx`
Expected: FAIL (`SqlCellHeader` not exported).

- [ ] **Step 3: Implement** — add a small focused subcomponent and render it for SQL cells. Use the existing token accent (`CELL_LANGUAGE_TOKENS.sql.accent`) for styling consistency:

```tsx
export function SqlCellHeader({
  relation,
  onRelationChange,
}: {
  relation: string;
  onRelationChange: (next: string) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <span className="inline-flex items-center gap-1 rounded-full border border-amber-200 bg-amber-50 px-2 py-0.5 font-mono text-[10px] text-amber-700">
        <span aria-hidden="true">{"⛃"}</span> kernel session
      </span>
      <label className="ml-auto flex items-center gap-1 font-mono text-[10.5px] text-gray-500">
        <span aria-hidden="true">{"→"}</span>
        <span className="sr-only">relation</span>
        <input
          aria-label="relation"
          className="w-28 rounded bg-amber-100 px-1.5 py-0.5 font-semibold text-amber-900"
          onChange={(e) => onRelationChange(e.target.value)}
          placeholder="relation"
          value={relation}
        />
      </label>
    </div>
  );
}
```

Render `SqlCellHeader` in the cell header when `cellLanguageId(cell) === "sql"`. Wire
`relation` from the cell's existing `dag.produces[0].port` and `onRelationChange` to the
existing store mutator that updates `dag.produces` (the same path Python produced-port
cells use). If no such mutator is exposed to this component, emit `scope_drift` per the
checkpoint above rather than reaching into the store here.

- [ ] **Step 4: Run to verify pass**

Run: `scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.sql.test.tsx`
Run: `scripts/spur-pnpm run typecheck`
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx \
        crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.sql.test.tsx
git commit -m "feat(spur-notebook): sql-cell-duckdb SQL cell header chrome"
```

---

## Self-Review

**Spec coverage:**
- AC1 (run SQL, inline grid) → BT2 (transpile/run) + FT3 (preview opt-in) + FT2 (highlight).
- AC2 (chained SQL / shared session) → BT2 (shared default connection + produced global).
- AC3 (DataFrame interop both ways) → BT2 (Arrow global binding + replacement scans).
- AC4 (named port + lineage + cascade) → FT3 (relation→`dag.produces`) + BT3 (auto deps).
- AC5 (switcher, token, highlight, pill) → FT1 + FT2 + FT3.
- AC6 (tests, green build) → every task is TDD with spur-cargo / spur-pnpm commands.
- Open question (relation-name default): FT3 defaults to the existing `dag.produces[0].port` or empty (anonymous) — no invented default; documented.
- Open question (INSTALL gating): deferred per spec; not in any task (correctly out of scope for v1).

**Placeholder scan:** no TBD/TODO; every code step has real code and exact run commands.

**Type consistency:** `CodeType::Sql` (BT1) ↔ `code_type_kernelspec("sql")` and `kernelspec_for(CodeType::Sql)` (BT2) ↔ `"sql"` binding member used by `cellLanguage.ts`/`CellInput.tsx` (FT1/FT2). `PortSpec { port, repr }` (FT3) matches `backend/notebook.rs` `PortSpec`.

**DAG validation:** BT1 root; BT2→BT3 chain; FT1→FT3; FT2 independent after BT1. No cycles. Wide after the single root.

**beads compatibility:** each task has a unique id, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary; high-drift task (FT3) carries an explicit scope-drift checkpoint.

**Suggested worker:** codex for all tasks (per user direction); FT3 is the most involved and the most likely to signal `scope_drift` if store wiring is missing.

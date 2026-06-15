# DuckDB SQL Cell — First-Class Cell Type (Design Spec)

- **Date:** 2026-06-15
- **Design epic:** `bd-2rgcu`
- **Plan id:** `sql-cell-duckdb-2026-06-15`
- **Status:** Design approved through Approach A+1 + UI/UX (pending spec review)
- **UI/UX artifact:** rendered HTML cell in the design notebook (`Untitled139.ipynb`, cell 2) — the source-of-truth visual.
- **Driver:** authors should write DuckDB SQL directly in a notebook cell, with no Python boilerplate, and SQL cells in the same kernel must reuse one shared session (global-space reuse), inspired by the Tauri SQL plugin.

## Problem

Today SQL is run *indirectly* through a SPUR-managed Python cell:
`datasource_setup_source` (`crates/spur-notebook/src/mcp/mod.rs:980`) emits
`import duckdb; duckdb.sql(...)` and attaches datasources as DuckDB views. Authors
who want SQL must drop into Python and wrap every statement. There is no
first-class SQL cell, no SQL syntax highlighting, no inline result grid, and no
direct way to chain SQL cell to SQL cell.

The notebook is already polyglot: every code cell carries
`cell.metadata.spur.code_type ∈ {python, javascript, rust, go, spur}` and the DAG
runner routes each to a kernelspec via `code_type_kernelspec`
(`crates/spur-notebook/src/dag/cell_runner.rs:191`). A polyglot chip switcher UI
already exists (`cellLanguage.ts`, `CellLanguageMenu.tsx`, per-language CodeMirror
highlighting; see `docs/superpowers/specs/2026-06-03-notebook-polyglot-cell-ui-design.md`).
A SQL cell is therefore a natural *new `code_type`*, not a new kernel.

## Industry scan (why not a separate SQL kernel)

| Tool | SQL cell model | Cross-cell state | Python interop |
|---|---|---|---|
| marimo (reactive DAG, closest analog) | `mo.sql()`, DuckDB by default, reactive node | shared connection | result is a DataFrame; queries Python frames by name |
| Hex | "Dataframe SQL" / "Chained SQL", DuckDB under the hood | auto-CTE chaining | queries any prior DataFrame |
| Deepnote | SQL block to DataFrame, query chaining | reuse query previews | result is a DataFrame |
| JupySQL (`%sql`) | magic inside the Python kernel, native DuckDB conn | reused conn alias | replacement scans on local frames |
| xeus-sql | true native SQL kernel | session state, SQL-only | none (isolated) |

A genuine native SQL kernel (`xeus-sql`) exists but is **isolated** from Python and
treats DuckDB as second-class. Every reactive/dataflow notebook (marimo, Hex,
Deepnote, JupySQL) instead runs SQL **inside the language kernel against one
persistent DuckDB connection**. That model is the only one that satisfies the
global-space-reuse requirement *and* unlocks SQL/Python DataFrame interop.

## Approach A+1 — native SQL cell, in-kernel shared DuckDB connection

- **UI:** SQL is a first-class `code_type = "sql"` with its own token (glyph `SQL`,
  label `DuckDB`, DuckDB-amber accent `#F6BD3B`), SQL highlighting, accent bar, and
  tinted gutter. It feels native next to Python/JS/Rust/Go.
- **Engine:** all SQL cells share **one** persistent DuckDB connection living in the
  kernel global namespace. DuckDB's module-level default connection
  (`duckdb.sql(...)`, already used by `datasource_setup_source`) **is** a
  process-global shared connection, so `CREATE VIEW` / `CREATE TABLE` / `INSTALL`
  in one SQL cell is visible in the next, for free.
- **Kernel:** `code_type_kernelspec("sql")` routes to **`python3`** (the shared
  kernel). SQL cells require a Python kernel in the notebook; the switcher surfaces
  this when no Python kernel is present.
- **Interop ("everything as table"):** SQL cells query datasource views *and*
  upstream Python DataFrames/Arrow ports by bare name (DuckDB replacement scans /
  `register`). The result is published as a named DAG **port** (Arrow) *and* bound
  as a DataFrame in kernel globals, consumable by downstream Python *and* SQL cells.
- **Execution:** the SQL cell **transpiles** to a `duckdb` call on the shared
  connection at run time, inheriting the existing DAG/lineage/port/cascade machinery
  with no new execution path.

Rejected: separate native SQL kernel (breaks shared global space + Python interop);
Tauri-host SQL plugin (bypasses the DAG/lineage entirely).

## UI/UX design

The rendered artifact (design notebook, cell 2) shows a 4-cell slice: a Python cell
producing `matches`, a hero SQL cell, a chained SQL cell, and a Python consumer.

1. **Chip + switcher.** The SQL token joins the existing polyglot menu
   (`CellLanguageMenu.tsx`). One click flips a cell to/from DuckDB SQL. The menu order
   is Python / JavaScript / Rust / Go / **DuckDB SQL**, divider, Markdown / Raw
   (disabled) / AI Agent (disabled, `bd-1bpb`).
2. **Accent + gutter.** Amber 3px left accent bar and amber-tinted execution marker,
   matching the polyglot identity system.
3. **`⛁ kernel session` pill.** Signals the cell reuses the one persistent in-kernel
   DuckDB connection. Communicates that catalog state persists across SQL cells.
4. **`→ relation` name affordance.** An inline-editable output name on the header
   right. Becomes the DAG port name and the kernel-global DataFrame name. Empty name =
   anonymous preview (renders, publishes nothing).
5. **Result grid.** The result renders inline as a Perspective grid with a
   `rows · cols · timing · engine` footer and a `materialized → <port>` chip.
6. **Chained SQL.** A later SQL cell reads a prior relation directly from the shared
   catalog; the lineage line shows auto-tracked DAG dependencies.

### `cellLanguage.ts` token

```
sql -> { glyph: "SQL", label: "DuckDB", kernelspec: "python3" (shared),
         accent: "#F6BD3B", highlight: @codemirror/lang-sql (PostgreSQL dialect) }
```

Accent contrast: amber `#F6BD3B` is bright; the existing JS token is a *dark* muted
gold (`#8A6D00`), so the two read as distinct (bright vs dark). Confirm in-app.

## File map (implementation; for the eventual plan, not this design)

**Frontend (`jute-notebook`):**
- `src/ui/notebook/cellLanguage.ts` — add the `sql` token (+ test).
- `src/ui/notebook/CellLanguageMenu.tsx` — add the DuckDB SQL option (+ test).
- `src/ui/notebook/CellInput.tsx` — select `@codemirror/lang-sql` when `code_type === "sql"`.
- `src/ui/notebook/NotebookCells.tsx` — SQL chrome: `⛁` session pill + `→ relation`
  name input; reuse the existing port/Perspective result grid (+ test).
- `src/stores/notebook.ts` — `CellType` stays `code | markdown`; `code_type` carries
  `"sql"`; the relation name is stored in `cell.metadata.spur` DAG produced-port.
- `package.json` — add `@codemirror/lang-sql`.
- `src/bindings/CodeType.ts` — regenerate via `ts-rs-export` (adds `Sql`).

**Backend (`spur-notebook` core):**
- `src/backend/notebook.rs` — add `CodeType::Sql` (+ `code_type_for_spec` inverse).
- `src/dag/cell_runner.rs` — `code_type_kernelspec("sql") -> python3`; transpile the
  SQL source to a `duckdb` call on the shared connection; bind the result to the
  named port (Arrow) + a kernel-global DataFrame.
- Dependency/lineage extraction — derive referenced relations from the SQL (DuckDB
  `json_serialize_sql` AST, or `duckdb_extract_statements`) to wire DAG edges and the
  replacement-scan registration set.

## Scope / YAGNI

**In:** DuckDB-only SQL cell; in-kernel shared connection; replacement-scan interop;
result port + DataFrame; chip/switcher/highlighting/result-grid; chained SQL.

**Deferred:** non-DuckDB engines (Postgres/MySQL/SQLite à la Tauri plugin);
SQL-only notebooks with no Python kernel (A+2); parameterized/`f-string`-style SQL
interpolation of Python values; write-back / DML governance.

## Risks

- **Dependency extraction from SQL.** Lineage and replacement-scan registration need
  the referenced relation names. Prefer DuckDB's own parser (`json_serialize_sql`)
  over a hand-rolled regex; a wrong set silently breaks cascade ordering.
- **Replacement-scan scope.** DuckDB replacement scans read the executing frame's
  locals/globals. Transpilation must run the `duckdb.sql(...)` in a scope where the
  referenced kernel-global frames are visible, or explicitly `register` them.
- **No Python kernel.** A+1 needs `python3`; the switcher must clearly surface that a
  SQL cell requires a Python kernel rather than failing opaquely.
- **Accent collision.** Amber vs JS gold must stay visually distinct in-app.
- **Large results.** Reuse the existing Arrow-port + Perspective windowing; do not
  inline-materialize unbounded result sets into the DOM.

## Acceptance criteria

1. A cell with `code_type = "sql"` runs DuckDB SQL with no visible Python and renders
   an inline result grid.
2. A second SQL cell reads a relation created by the first without re-declaring it
   (shared connection / global-space reuse).
3. A SQL cell queries an upstream Python DataFrame by bare name; a downstream Python
   cell consumes the SQL result as a DataFrame (bidirectional interop).
4. Naming the `→ relation` publishes a DAG port + kernel-global DataFrame; lineage
   edges are auto-tracked and cascade re-runs dependents.
5. The switcher lists DuckDB SQL; SQL cells show the amber token, accent bar, tinted
   gutter, and `⛁` session pill; SQL syntax is highlighted.
6. Tests: token module, switcher menu, highlighting selection (frontend); kernelspec
   routing, transpile, port materialization, dependency extraction (backend). Green
   via `scripts/spur-cargo test -p spur-notebook`, `scripts/spur-pnpm test`, and
   `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`. Never bare
   cargo / pnpm.

## Open questions

- Relation-name default: auto-derive from cell position (`cell_2`) or require explicit
  naming for a published port?
- Should `INSTALL`/`LOAD` of DuckDB extensions in a SQL cell be capability-gated by the
  app manifest, or always allowed in-kernel?

# Repository Guidelines

## Project Structure & Module Organization
SPUR is a Rust workspace (18 crates under `crates/` + `xtask/`). Keep changes scoped to one crate unless a cross-crate refactor is clearly necessary.

**Core:**
- `crates/spur-cli`: binary entry point
- `crates/spur-core`: orchestration, review loop, lineage, event pipeline
- `crates/spur-acp`: ACP client, transports (stdio/native/cli-wrap), event types
- `crates/spur-tui`: `ratatui` interface, views, components
- `crates/spur-mcp`: MCP server exposing delegation tools to the brain

**Data & Analytics:**
- `crates/spur-context`: DuckDB analytics engine + per-agent log extractors
- `crates/spur-cost`: pricing registry + SQLite session ledger
- `crates/spur-graph`: tree-sitter code graph, stable IDs, incremental rebuild
- `crates/spur-analyst`: DuckDB graph index for SQL queries over code/plan data

**Infrastructure:**
- `crates/spur-worktree`: git worktree creation, isolation, flock liveness, cleanup
- `crates/spur-pm`: project management adapters (beads, GitHub)
- `crates/spur-blob-store`: content-addressed delegation outcome artifacts
- `crates/spur-telemetry`: tracing/logging infrastructure

**Frontends & Bridges:**
- `crates/spur-interactive`: frontend bridge for non-TUI clients
- `crates/spur-bot`: Telegram bot frontend

The Jupyter-style notebook UI, `jute-notebook`, and `rest-table-gateway` source now live in the standalone `getspur/spur-notebook` repository. This workspace consumes the green standalone notebook as an external artifact.

**Licensing & Testing:**
- `crates/spur-license` / `crates/spur-license-admin`: tier/feature-key registry
- `crates/spur-test-madsim`: simulation-based testing harness

Source lives in each crate’s `src/`. Integration tests are primarily in `crates/spur-acp/tests`, `crates/spur-core/tests`, `crates/spur-tui/tests`, and `crates/spur-cli/tests`. Specs and implementation plans live in `docs/superpowers/specs/` and `docs/superpowers/plans/`.

## Code Retrieval & Exploration

Treat the repository code graph as the first-class retrieval layer for code work. Choose tools based on question shape, but **start with `knowledge_context_pack` for discovery and exploration.**

### First-Class: `knowledge_context_pack` — Discovery & One-Shot Retrieval

**`knowledge_context_pack` is the default entry point for exploration — reach for it first.** For any "where does X live / what's around this concept / get me oriented" question, issue one pack call before hand-chaining `code_*`. It returns a bounded, one-shot evidence pack combining:
- **BM25 retrieval** over symbol token text AND documentation/spec/plan section bodies
- **Scorecard signals** (pagerank, churn, posture) to surface high-value, load-bearing symbols
- **Exact graph context** — caller/callee impact summaries with popular-sink boundaries already applied
- **Staleness metadata** to detect when the analyst index lags the working tree
- **`recommended_next_tools`** — pre-filled `code_*` selectors that hand you straight to precise follow-up

One call replaces several rounds of manual search.

**When to use:**
- You don't know what to look for — "What's around this concept?" / "Where is X?"
- Quick orientation before diving into specific symbols
- One-shot bounded evidence gathering without manual tool chaining

**Known caveat (temporary):** code retrieval is BM25-only today, so recall is identifier-vocabulary-dependent — a concept whose words don't appear in symbol names may return few or no code hits (doc grounding stays strong). Read the BM25 scores and `confidence`; when code recall looks thin, broaden with `code_symbol_search` or `spur-analyst`. A dedicated workstream is adding semantic (vector/ANN) retrieval shortly, which lifts this ceiling — **prefer `knowledge_context_pack` as the first move regardless.**

**Example:**
```rust
knowledge_context_pack({
  "query": "delegation error handling",
  "intent": "change",      // explain|change|review|debug|plan
  "scope": "code",         // all|docs|code|graph
  "limit": 8,              // 1-20, default 8
  "include_tests": false,  // filter test files
  "max_symbol_bodies": 3   // 0-5, fetch source for top symbols
})
```

### Precise Follow-up: `code_*` — Symbol Work

**`code_*` is the precise substrate for exact symbol work** — ideally seeded from a `knowledge_context_pack` selector rather than re-resolved by name:

- **`code_symbol_search`** — Fuzzy/substring symbol discovery
- **`code_resolve`** — Exact symbol lookup from stable IDs or qualified names
- **`code_read_symbol`** — Read source with context lines, file OID tracking
- **`code_callers`** / **`code_callees`** — Impact analysis with unresolved edge detection
- **`code_subgraph`** — Neighborhood maps (use sparingly, cap radius at 2)

**When to use:**
- You know the symbol name or have a specific target (often from a pack selector)
- Need exact source code, caller/callee impact, or call tracing
- Verifying symbol signatures, types, or implementations

### Specialist: `spur-analyst` — Deep Analysis

**Use `spur-analyst` tools for complex queries requiring SQL, aggregation, or graph algorithms.** The analyst layer exposes the full DuckDB database with:
- **Aggregation** — GROUP BY, window functions, statistical queries
- **Temporal analysis** — Commit history, file churn, co-change patterns
- **Reachability** — Recursive CTEs, shortest paths, connected components
- **Graph algorithms** — PageRank, betweenness centrality, community detection

**When to use:**
- "Which symbols have the highest churn in this module?"
- "Show me the commit history for this file over the last 90 days"
- "Find all transitive callers within 3 hops"
- "What's the co-change pattern between these modules?"

### Fallback: Native Tools

Fall back to native tools such as `rg`, `sed`, `cat`, or direct file reads only when:
- The graph tools do not expose the needed shape of data (e.g., full raw markdown, shell scripts, config files)
- The graph is unavailable or stale for the file in question
- You need exact working-tree bytes/diffs for untracked or recently modified files

When falling back, keep the search scoped and note the reason.

## Build, Test, and Development Commands

**Always build and test through `scripts/spur-cargo`, never plain `cargo`.** For any compile-heavy work — `build`, `check`, `test` (including end-to-end suites), `clippy`, `doc`, `clean` — `spur-cargo` is the required entry point. Agents frequently default to bare `cargo` out of habit; do not. Bare `cargo` compiles into the local `target/`, which under the agent sandbox routinely fails (EPERM writing build artifacts on provenance-tagged files, and limited local disk that can't fit heavy C/C++ deps like `duckdb`). `spur-cargo` sidesteps both.

**Remote is the default for heavy compiles; the script handles fallback.** The **remote-default** subcommands — `build`/`check`/`test`/`doc`/`clean` — dispatch to the GCP build VM with its GCS-backed sccache by default (no opt-in marker needed) — faster, and immune to local-target permission/disk problems. They fall back to local cargo **only** when the VM itself is unreachable (build.sh exit `200`); genuine build/test failures (cargo exit `1`/`100`/`101`) propagate unchanged, so a red remote test is a real failure — don't re-run it locally to "get it to pass." Per-invocation overrides: `SPUR_REMOTE=0` forces local (any subcommand, e.g. interactive debugging or a machine without the VM configured); `SPUR_REMOTE=1` forces remote (any compile-capable subcommand, incl. `clippy` and `run`).

**`clippy`/`fmt` auto-run local; `run` is remote by default.** `clippy` runs locally for fast incremental feedback and `fmt` never goes remote. **`run` defaults to remote** like the heavy compiles — running a binary means compiling it first, so it compiles AND executes on the VM (a sandbox never pays the heavy bundled-DuckDB compile locally), then **`scripts/gcp-build/build.sh` syncs the worktree files the binary wrote back to the local tree.** To keep that capture clean the binary runs in a **private per-invocation copy of the worktree** (`/mnt/cargo/spur-runs/<key>.<pid>.<epoch>`, compile cache reused via a `target/` symlink) so a concurrent build on the shared tree can't leak its writes into your sync-back. Caveat: only paths *inside the worktree* are synced back — a binary that writes out-of-tree (e.g. `/tmp`) leaves its output on the VM, so point generated output at a worktree-relative dir. For an interactive run (TUI, a local server/port, anything needing live stdin or localhost) use **`SPUR_REMOTE=0 scripts/spur-cargo run …`** to force it local. **In a sandboxed agent this matters:** local `clippy` writes to `target/` and will hit the same EPERM/disk problems as bare `cargo`, so when you need a clean lint pass from inside the sandbox run **`SPUR_REMOTE=1 scripts/spur-cargo clippy …`** to force it onto the VM. Interactive/local development can use plain `scripts/spur-cargo clippy …`.

In local mode (fallback, or when remote is disabled) the wrapper also keeps sccache's `SCCACHE_BASEDIRS` in sync with the current set of git worktrees for cross-worktree cache reuse — a no-op when nothing changed, and it refuses to restart sccache while rustc is running. See `docs/rca/2026-04-27-sccache-worktree-cache-miss.md` and `scripts/gcp-build/` for the remote pipeline.

- `scripts/spur-cargo build --workspace`: build all crates (with sccache sync)
- `scripts/spur-cargo test --workspace`: run the full test suite
- `scripts/spur-cargo test -p spur-tui`: run tests for one crate while iterating
- `scripts/spur-cargo clippy --workspace -- -D warnings`: enforce lint-clean code (local-first; prefix `SPUR_REMOTE=1` to force remote from a sandboxed agent)
- `scripts/spur-cargo fmt --all`: apply workspace formatting
- `scripts/spur-cargo run -p spur-cli -- --help`: run a binary (compiles + executes on the VM, outputs synced back); prefix `SPUR_REMOTE=0` to keep a quick interactive run local
- `scripts/sccache-worktree.sh`: sccache rustc-wrapper that dynamically normalizes paths per worktree
- `scripts/sccache-sync-basedirs.sh`: legacy sync script (superseded by `sccache-worktree.sh`)

**Notebook frontend source now lives in `getspur/spur-notebook`.** `scripts/spur-pnpm` is only a post-split compatibility wrapper in this repo. Without `SPUR_NOTEBOOK_REPO`, it exits with migration guidance. With `SPUR_NOTEBOOK_REPO=/path/to/spur-notebook`, it forwards locally to `pnpm --dir "$SPUR_NOTEBOOK_REPO/jute-notebook" ...`; it no longer dispatches through `build.sh --pnpm`.

- `SPUR_NOTEBOOK_REPO=/path/to/spur-notebook scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx`: forward one notebook frontend test to a standalone checkout
- `SPUR_NOTEBOOK_REPO=/path/to/spur-notebook scripts/spur-pnpm run typecheck`: forward typecheck to a standalone checkout
- Run remote notebook frontend workflows from the standalone repo's own tooling.
- The monorepo `lint-invariants` workflow checks SDK fixtures against the private standalone repo and requires a `SPUR_NOTEBOOK_CHECKOUT_TOKEN` repository secret with read access to `getspur/spur-notebook`.

## Coding Style & Naming Conventions
Use Rust 2021 idioms with `cargo fmt` formatting. Follow existing naming: modules and functions in `snake_case`, types and traits in `CamelCase`, constants in `SCREAMING_SNAKE_CASE`. Prefer small, crate-local changes over broad rewrites. Avoid introducing new crate dependencies without explicit justification.

## Testing Guidelines
Bug fixes should follow TDD cadence: add a failing `test(...)` commit first, then the `fix(...)` commit. New ACP event variants or envelope fields require round-trip serialization tests modeled on `crates/spur-acp/tests/executor_events_roundtrip.rs`. When changing config validation, run `scripts/spur-cargo test -p spur-acp`.

## Commit & Pull Request Guidelines
Commit format is:

`<type>(<scope>): <sub-id> <short imperative>`

Example: `fix(spur-tui): S1.c cap per-frame event drain at 8`

Valid types include `feat`, `fix`, `test`, `docs`, `refactor`, and `chore`. Keep subjects under 72 characters. PRs should explain the problem, summarize the change, link the plan or issue when applicable, and note any user-visible TUI behavior changes with screenshots or terminal captures.

Whenever you add, change, or delete code or documentation, commit the finished change with a meaningful message that describes the intent.

## Plan-Driven Workflow
Non-trivial work should flow from spec to plan to implementation. Before executing an older plan, verify it against current code and pay attention to established invariants around broadcast sizing, TUI event draining, ACP sequencing, and notification grace windows.

---

## SPUR Signal Conventions (v1)

SPUR uses sentinel conventions embedded in beads comment bodies to propagate structured metadata between brain, workers, and the reconciler. Workers and the MCP server emit these signals; the brain consumes them.

> **Agent guidance:** The bundled `spur-way` skill establishes beads as the sole source of truth and mandates the `INTENT → ACTION → RECORD` primitive for every transaction. The `worker-signals`, `brain-review-gate`, `beads-lifecycle`, and `plan-task-discipline` skills provide detailed enforcement guidance for these conventions. See `crates/spur-core/src/skills/` for the authoritative skill bodies.

### `[[spur-signal v1]]` — Worker-to-Brain Signals

Workers emit signals as sentinel-fenced JSON inside a beads comment, plus a `signal:<kind>` label. The brain parses comments to derive signals.

**Comment format:**
```
[[spur-signal v1]]
{
  "signal_id": "<uuid-v4>",
  "kind": "scope_drift",
  "severity": 0.82,
  "reason": "auth refactor pulls in 4 new subsystems",
  "estimated_subtasks": 3
}
```

**Label format:** `signal:<kind>`, optionally bucketed as `signal:<kind>:<bucket>` (e.g., `signal:scope-drift:high`).

The brain MUST deduplicate signals by `signal_id` across polls — workers may emit the same signal multiple times.

### `[[spur-audit v1]]` — Audit Trail

SPUR emits audit breadcrumbs as sentinel comments on beads issues. This replaces `br audit record` (which drops data on persist).

**Audit sentinel variants:**

| Kind | Purpose | Key fields |
|---|---|---|
| `plan-submit` | Plan persisted | `plan_id`, `epic_issue_id`, `task_ids[]` |
| `dispatch` | Task dispatched to worker | `delegation_id`, `worker`, `attempt` |
| `completion` | Worker completed | `delegation_id`, `worker_branch`, `result_summary` |
| `approval` | Brain approved | `delegation_id` |
| `rejection` | Brain rejected | `delegation_id`, `feedback` |

**Comment format:**
```
[[spur-audit v1]]
{
  "kind": "dispatch",
  "delegation_id": "del-A",
  "worker": "codex",
  "attempt": 1
}
```

### Label Vocabulary

| Label | Purpose | Set by |
|---|---|---|
| `spur:plan-id:<id>` | Plan ID scope | brain at submit |
| `spur:plan-task-id:<id>` | Task ID scope | brain at submit |
| `spur:plan-complete` | Epic fully persisted | server on epic creation |
| `spur:agent:<name>` | Worker agent | brain at submit |
| `spur:source-issue:<id>` | Source issue reference | server at submit |
| `delegation-id:<id>` | ACP delegation | reconciler on dispatch |
| `signal:<kind>` | Signal present | worker via MCP tool |
| `signal:<kind>:<bucket>` | Signal severity bucket | worker via MCP tool |
| `signal:late-arrival` | Signal after terminal | brain signal handler |
| `spur:mutation-id:<compact-uuid>` | Mutation batch children | brain mutation executor (create path) |
| `spur:superseded-by:<child-id>` | Parent task split marker (one per child) | brain mutation executor |
| `spur:signal-processed:<compact-uuid>` | Signal consumed marker | brain mutation executor (label-add path) |
| `ready-for-review` | Explicit review-ready | reconciler on completion (**not yet wired — see spec §Known Correctness Gaps**) |

All labels must use br-legal characters: `[A-Za-z0-9_:-]+`. `br create --label`
enforces a 50-character cap; `br label add` does not. Constructors used at
create time (`mutation_id_label`) use the compact (hyphen-free) UUID suffix
to stay under the cap. See `crates/spur-mcp/src/plan/labels.rs` for the
authoritative list.

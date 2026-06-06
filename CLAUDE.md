# Repository Guidelines

## Project Structure & Module Organization
SPUR is a Rust workspace with eight crates under `crates/`. Keep changes scoped to one crate unless a cross-crate refactor is clearly necessary.

- `crates/spur-acp`: ACP client, transport, event types
- `crates/spur-core`: orchestration, review loop, lineage
- `crates/spur-tui`: `ratatui` interface, views, components
- `crates/spur-cli`: binary entry point
- `crates/spur-mcp`, `spur-pm`, `spur-worktree`, `spur-cost`: integrations and support services

Source lives in each crate’s `src/`. Integration tests are primarily in `crates/spur-acp/tests`, `crates/spur-core/tests`, `crates/spur-tui/tests`, and `crates/spur-cli/tests`. Specs and implementation plans live in `docs/superpowers/specs/` and `docs/superpowers/plans/`.

## Code Retrieval & Exploration

Treat the repository code graph as the first-class retrieval layer for code work. For code discovery, symbol lookup, reading symbol bodies, caller/callee impact analysis, and semantic searches over code or docs, use the available `code_*` tools first whenever they can answer the question.

Fall back to native tools such as `rg`, `sed`, `cat`, or direct file reads only when the graph tools do not expose the needed shape of data (for example, full raw markdown/shell-file reads), the graph is unavailable or stale for the file in question, or you need exact working-tree bytes/diffs. When falling back, keep the search scoped and note the reason.

## Build, Test, and Development Commands

**Always build and test through `scripts/spur-cargo`, never plain `cargo`.** For any compile-heavy work — `build`, `check`, `test` (including end-to-end suites), `clippy`, `doc`, `clean` — `spur-cargo` is the required entry point. Agents frequently default to bare `cargo` out of habit; do not. Bare `cargo` compiles into the local `target/`, which under the agent sandbox routinely fails (EPERM writing build artifacts on provenance-tagged files, and limited local disk that can't fit heavy C/C++ deps like `duckdb`). `spur-cargo` sidesteps both.

**Remote is the default for heavy compiles; the script handles fallback.** The **remote-default** subcommands — `build`/`check`/`test`/`doc`/`clean` — dispatch to the GCP build VM with its GCS-backed sccache by default (no opt-in marker needed) — faster, and immune to local-target permission/disk problems. They fall back to local cargo **only** when the VM itself is unreachable (build.sh exit `200`); genuine build/test failures (cargo exit `1`/`100`/`101`) propagate unchanged, so a red remote test is a real failure — don't re-run it locally to "get it to pass." Per-invocation overrides: `SPUR_REMOTE=0` forces local (any subcommand, e.g. interactive debugging or a machine without the VM configured); `SPUR_REMOTE=1` forces remote (any compile-capable subcommand, incl. `clippy`).

**`clippy` and `fmt`/`run` auto-run local.** Lint, format, and run are lightweight and are auto-forced local even though remote is the default for heavy compiles — `clippy` runs locally for fast incremental feedback; `fmt`/`run` never go remote. **In a sandboxed agent this matters:** local `clippy` writes to `target/` and will hit the same EPERM/disk problems as bare `cargo`, so when you need a clean lint pass from inside the sandbox run **`SPUR_REMOTE=1 scripts/spur-cargo clippy …`** to force it onto the VM. Interactive/local development can use plain `scripts/spur-cargo clippy …`.

In local mode (fallback, or when remote is disabled) the wrapper also keeps sccache's `SCCACHE_BASEDIRS` in sync with the current set of git worktrees for cross-worktree cache reuse — a no-op when nothing changed, and it refuses to restart sccache while rustc is running. See `docs/rca/2026-04-27-sccache-worktree-cache-miss.md` and `scripts/gcp-build/` for the remote pipeline.

- `scripts/spur-cargo build --workspace`: build all crates (with sccache sync)
- `scripts/spur-cargo test --workspace`: run the full test suite
- `scripts/spur-cargo test -p spur-tui`: run tests for one crate while iterating
- `scripts/spur-cargo clippy --workspace -- -D warnings`: enforce lint-clean code (local-first; prefix `SPUR_REMOTE=1` to force remote from a sandboxed agent)
- `scripts/spur-cargo fmt --all`: apply workspace formatting
- `scripts/spur-cargo run -p spur-cli -- --help`: inspect CLI entry points locally
- `scripts/sccache-worktree.sh`: sccache rustc-wrapper that dynamically normalizes paths per worktree
- `scripts/sccache-sync-basedirs.sh`: legacy sync script (superseded by `sccache-worktree.sh`)

**For the notebook frontend, build and test through `scripts/spur-pnpm`, never plain `pnpm`.** The notebook UI lives in `crates/spur-notebook/jute-notebook`; `spur-pnpm` is the pnpm analog of `spur-cargo`. It dispatches the command to the GCP build VM through the same sync path (`build.sh --pnpm`), where pnpm uses a **shared content-addressable store** at `/mnt/cargo/pnpm-store` and a **per-worktree `node_modules`** symlinked to `/mnt/cargo/pnpm-nm/<worktree-key>` (both on `/mnt/cargo` so packages hard-link from the store instead of re-downloading). The first worktree populates the store; every worktree after that installs link-only in seconds — **no per-worktree `pnpm install` from scratch.** It follows the same fallback contract as `spur-cargo`: falls back to local pnpm **only** when the VM is unreachable (exit `200`); a genuine test/typecheck failure propagates unchanged. `SPUR_REMOTE=0` forces local; pin the activated pnpm with `SPUR_PNPM_VERSION` (default `10.28.2`). Lockfile handling is automatic — a tracked `pnpm-lock.yaml` triggers `--frozen-lockfile`, otherwise pnpm imports the existing `package-lock.json`.

- `scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx`: run one notebook frontend test file on the VM
- `scripts/spur-pnpm run typecheck`: typecheck the notebook frontend remotely
- `SPUR_REMOTE=0 scripts/spur-pnpm test ...`: force local pnpm (no VM)

## Coding Style & Naming Conventions
Use Rust 2021 idioms with `cargo fmt` formatting. Follow existing naming: modules and functions in `snake_case`, types and traits in `CamelCase`, constants in `SCREAMING_SNAKE_CASE`. Prefer small, crate-local changes over broad rewrites. Avoid introducing new crate dependencies without explicit justification.

## Testing Guidelines
Bug fixes should follow TDD cadence: add a failing `test(...)` commit first, then the `fix(...)` commit. New ACP event variants or envelope fields require round-trip serialization tests modeled on `crates/spur-acp/tests/executor_events_roundtrip.rs`. When changing config validation, run `cargo test -p spur-acp`.

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

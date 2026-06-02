# Repository Guidelines

## Project Structure & Module Organization
SPUR is a Rust workspace with eight crates under `crates/`. Keep changes scoped to one crate unless a cross-crate refactor is clearly necessary.

- `crates/spur-acp`: ACP client, transport, event types
- `crates/spur-core`: orchestration, review loop, lineage
- `crates/spur-tui`: `ratatui` interface, views, components
- `crates/spur-cli`: binary entry point
- `crates/spur-mcp`, `spur-pm`, `spur-worktree`, `spur-cost`: integrations and support services

Source lives in each crate’s `src/`. Integration tests are primarily in `crates/spur-acp/tests`, `crates/spur-core/tests`, `crates/spur-tui/tests`, and `crates/spur-cli/tests`. Specs and implementation plans live in `docs/superpowers/specs/` and `docs/superpowers/plans/`.

## Build, Test, and Development Commands

**Always build and test through `scripts/spur-cargo`, never plain `cargo`.** For any compile-heavy work — `build`, `check`, `test` (including end-to-end suites), `clippy`, `doc`, `clean` — `spur-cargo` is the required entry point. Agents frequently default to bare `cargo` out of habit; do not. Bare `cargo` compiles into the local `target/`, which under the agent sandbox routinely fails (EPERM writing build artifacts on provenance-tagged files, and limited local disk that can't fit heavy C/C++ deps like `duckdb`). `spur-cargo` sidesteps both.

**Priority: remote build/test first; the script already handles fallback.** When `<main_repo>/.spur/remote-cargo.enabled` exists (or `SPUR_REMOTE=1`), `spur-cargo` dispatches the compile-heavy subcommands to the GCP build VM with its GCS-backed sccache — faster, and immune to local-target permission/disk problems. It falls back to local cargo **only** when the VM itself is unreachable (build.sh exit `200`); genuine build/test failures (cargo exit `1`/`100`/`101`) propagate unchanged, so a red remote test is a real failure — don't re-run it locally to "get it to pass." Per-invocation overrides: `SPUR_REMOTE=1` forces remote (e.g. CI), `SPUR_REMOTE=0` forces local (e.g. `fmt`, `run`, interactive debugging).

In local mode (fallback, or when remote is disabled) the wrapper also keeps sccache's `SCCACHE_BASEDIRS` in sync with the current set of git worktrees for cross-worktree cache reuse — a no-op when nothing changed, and it refuses to restart sccache while rustc is running. See `docs/rca/2026-04-27-sccache-worktree-cache-miss.md` and `scripts/gcp-build/` for the remote pipeline.

- `scripts/spur-cargo build --workspace`: build all crates (with sccache sync)
- `scripts/spur-cargo test --workspace`: run the full test suite
- `scripts/spur-cargo test -p spur-tui`: run tests for one crate while iterating
- `scripts/spur-cargo clippy --workspace -- -D warnings`: enforce lint-clean code
- `scripts/spur-cargo fmt --all`: apply workspace formatting
- `scripts/spur-cargo run -p spur-cli -- --help`: inspect CLI entry points locally
- `scripts/sccache-worktree.sh`: sccache rustc-wrapper that dynamically normalizes paths per worktree
- `scripts/sccache-sync-basedirs.sh`: legacy sync script (superseded by `sccache-worktree.sh`)

## Coding Style & Naming Conventions
Use Rust 2021 idioms with `cargo fmt` formatting. Follow existing naming: modules and functions in `snake_case`, types and traits in `CamelCase`, constants in `SCREAMING_SNAKE_CASE`. Prefer small, crate-local changes over broad rewrites. Avoid introducing new crate dependencies without explicit justification.

## Testing Guidelines
Bug fixes should follow TDD cadence: add a failing `test(...)` commit first, then the `fix(...)` commit. New ACP event variants or envelope fields require round-trip serialization tests modeled on `crates/spur-acp/tests/executor_events_roundtrip.rs`. When changing config validation, run `cargo test -p spur-acp`.

## Commit & Pull Request Guidelines
Commit format is:

`<type>(<scope>): <sub-id> <short imperative>`

Example: `fix(spur-tui): S1.c cap per-frame event drain at 8`

Valid types include `feat`, `fix`, `test`, `docs`, `refactor`, and `chore`. Keep subjects under 72 characters. PRs should explain the problem, summarize the change, link the plan or issue when applicable, and note any user-visible TUI behavior changes with screenshots or terminal captures.

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

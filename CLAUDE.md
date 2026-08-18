# Repository Guidelines

Lean agent rules for this repo. Prefer `AGENTS.md` if both are loaded and they differ only in detail.

## Layout
Rust workspace (`crates/` + `xtask/`). One-crate scope unless a cross-crate change is required.

Core: `spur-cli`, `spur-core`, `spur-acp`, `spur-tui`, `spur-mcp`. Also: context/cost/graph/analyst, worktree/pm/blob-store/telemetry, interactive/bot, license, test-madsim.

Notebook UI: external `getspur/spur-notebook`. Specs/plans: `docs/superpowers/{specs,plans}/`.

## Skills

Foundation only by default (`catalog_only`): `skills-catalog`, `spur-way`, `code-explore`, `solve`, `brain-delegation`, `brain-review-gate`, `plan-task-discipline`, `worker-signals`, `beads-lifecycle`, `spur-analyst`.

Other skills: **`skill_navigate`** → **`skill_read`**. Prefer navigate over `skill_search`. No skill-dir walks, no task-specific materialization. Rollback: `projection_mode = "all_active"`. See `crates/spur-cli/assets/skills/skills-catalog/SKILL.md`.

## Code retrieval

1. **`knowledge_context_pack_2`** first (alias `knowledge_context_pack`)
2. **`code_*`** for exact symbols / impact
3. **`spur-analyst`** for SQL / graph algorithms
4. Native tools only if graph tools lack shape, are stale, or you need raw tree bytes

## Build & test

**Always** `scripts/spur-cargo` — never bare `cargo`.

- Remote default: `build` / `check` / `test` / `doc` / `clean` / `run`
- `SPUR_REMOTE=0` → local (interactive); `SPUR_REMOTE=1` → force remote (sandbox clippy)
- Red remote = real failure; don’t “fix” by re-running local
- `fmt` always local

```bash
scripts/spur-cargo test -p spur-tui
scripts/spur-cargo clippy --workspace -- -D warnings
scripts/spur-cargo e2e          # TUI e2e; artifacts → scripts/e2e/.artifacts/
scripts/spur-cargo coverage     # floors: workspace 75%, changed 85%
spur explore sync|list|add|remove|status
```

Cross/release: `zigbuild` (macOS), `xwin` (Windows), `cargo xtask dist`. Notebook: `SPUR_NOTEBOOK_REPO=… scripts/spur-pnpm …`.

## Style, test, commit

Rust 2021 + fmt; conventional naming; small diffs. TDD for bug fixes (`test` then `fix`). ACP events: round-trip tests. Commits: `<type>(<scope>): <sub-id> <short imperative>`. Commit finished work. Spec → plan → implement; re-verify old plans; keep broadcast / TUI drain / ACP seq / grace invariants.

## Signals

`spur-way` + foundation signal skills own formats and labels. Do not invent sentinel JSON or label vocab here. See `crates/spur-mcp/src/plan/labels.rs` when implementing server-side labels.

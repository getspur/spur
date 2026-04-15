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
- `cargo build --workspace`: build all crates
- `cargo test --workspace`: run the full test suite
- `cargo test -p spur-tui`: run tests for one crate while iterating
- `cargo clippy --workspace -- -D warnings`: enforce lint-clean code
- `cargo fmt --all`: apply workspace formatting
- `cargo run -p spur-cli -- --help`: inspect CLI entry points locally

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

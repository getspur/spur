# Spur Notebook Standalone Repository Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-16-spur-notebook-repo-split-design.md`
**Design epic:** `bd-1ox2l` (closed)

**Goal:** Split `crates/spur-notebook/` into standalone `getspur/spur-notebook` production ownership while preserving a blue/green SPUR launcher path.

**Architecture:** Phase 1 keeps the current in-tree notebook as blue and introduces a green external notebook channel. The standalone repo is bootstrapped from the existing subtree, initially using pinned git dependencies back to `getspur/spur`; SPUR then cuts over to green only after contract and packaging checks pass.

**Tech Stack:** Rust workspace crates, Tauri 2, Vite/React, DuckDB extension crate, GitHub repository bootstrap, SPUR MCP/socket notebook protocol.

---

## File Structure Mapping

- `crates/spur-core/src/notebook.rs`: notebook binary/channel resolution, MCP stdio command path.
- `crates/spur-tui/src/notebook_daemon.rs`: lazy launch diagnostics and selected-channel reporting.
- `crates/spur-tui/tests/notebook_daemon.rs`: socket/launcher behavior coverage.
- `xtask/src/main.rs`: install/build behavior that currently owns notebook artifacts.
- `Cargo.toml`: root workspace membership cutover after green validation.
- `scripts/spur-pnpm`: frontend wrapper currently assumes the in-tree notebook path.
- `docs/superpowers/plans/2026-06-16-spur-notebook-repo-split.md`: this plan.
- `crates/spur-notebook/**`: source subtree to extract into `getspur/spur-notebook`; do not delete until the green repo builds and blue/green tests pass.
- `getspur/spur-notebook` target repo: standalone workspace, CI, release docs, and notebook-owned source.

## Dependency DAG

```text
task-1-blue-green-resolver
  -> task-2-xtask-blue-green-install
  -> task-6-spur-green-contract-tests
  -> task-7-cutover-spur-workspace
  -> task-8-phase2-shared-crate-plan

task-3-create-standalone-repo
  -> task-4-standalone-manifests
  -> task-5-standalone-ci-release
  -> task-6-spur-green-contract-tests
```

`task-1` and `task-3` can start independently. `task-7` is the cutover task and must not start before both SPUR launcher coverage and standalone repo validation are complete.

### Task 1: Add Blue/Green Notebook Resolver

**Task ID:** `task-1-blue-green-resolver`

**Files:**
- Modify: `crates/spur-core/src/notebook.rs`
- Modify: `crates/spur-tui/src/notebook_daemon.rs`
- Modify: `crates/spur-tui/tests/notebook_daemon.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] SPUR supports an explicit notebook channel override for `blue`, `green`, and `auto`.
- [ ] `SPUR_NOTEBOOK_BIN` remains the highest-priority explicit binary override.
- [ ] Green can resolve an externally installed notebook binary or macOS app bundle without depending on `crates/spur-notebook/**`.
- [ ] Blue still resolves the current in-tree/sibling legacy behavior during the transition window.
- [ ] The launcher logs or exposes the selected channel, path, and reason before spawn.
- [ ] `scripts/spur-cargo test -p spur-core notebook_binary_path -- --nocapture` passes.
- [ ] `scripts/spur-cargo test -p spur-tui --test notebook_daemon -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: notebook resolver and launcher diagnostics only.
- OUT of scope: deleting `crates/spur-notebook/**`, changing `xtask`, or changing frontend code.
- If you need to alter install/build behavior, emit `scope_drift`; that belongs to Task 2.

**Implementation:**
- [ ] Add resolver types in `crates/spur-core/src/notebook.rs`, for example:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotebookChannel {
    Auto,
    Blue,
    Green,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookLaunchSelection {
    pub channel: NotebookChannel,
    pub path: PathBuf,
    pub reason: String,
}
```

- [ ] Parse `SPUR_NOTEBOOK_CHANNEL` with accepted values `auto`, `blue`, and `green`; return a clear error for any other value.
- [ ] Keep `SPUR_NOTEBOOK_BIN` as an absolute override and report it as the selected reason.
- [ ] Refactor `notebook_binary_path()` so it delegates to a selection function and preserves the current fallback order for blue/auto.
- [ ] In `crates/spur-tui/src/notebook_daemon.rs`, log the selected channel/path/reason immediately before `Command::new`.
- [ ] Add tests for channel parsing, green-only missing binary behavior, and `SPUR_NOTEBOOK_BIN` priority.
- [ ] Run:

```bash
scripts/spur-cargo test -p spur-core notebook_binary_path -- --nocapture
scripts/spur-cargo test -p spur-tui --test notebook_daemon -- --nocapture
```

- [ ] Commit:

```bash
git add crates/spur-core/src/notebook.rs crates/spur-tui/src/notebook_daemon.rs crates/spur-tui/tests/notebook_daemon.rs
git commit -m "feat(spur-core): bd-1ox2l add notebook channel resolver"
```

### Task 2: Update xtask For Blue/Green Install Behavior

**Task ID:** `task-2-xtask-blue-green-install`

**Files:**
- Modify: `xtask/src/main.rs`

**Depends on:** `task-1-blue-green-resolver`

**Acceptance Criteria:**
- [ ] `xtask install` can describe whether it installed blue artifacts or expects a green external notebook.
- [ ] Linux install no longer silently assumes green is unavailable.
- [ ] macOS install messaging explains the green standalone app path when selected.
- [ ] Existing blue install tests still pass during the transition window.
- [ ] `scripts/spur-cargo test -p xtask -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `xtask` install/build command construction and tests.
- OUT of scope: root workspace membership changes and deleting notebook crates.
- If you need to edit `Cargo.toml`, emit `scope_drift`; that belongs to Task 7.

**Implementation:**
- [ ] Add an install-channel parser for `--notebook-channel blue|green|auto`, defaulting to `auto`.
- [ ] For `blue`, preserve the existing in-tree notebook build/install behavior.
- [ ] For `green`, skip in-tree notebook build and print the expected external installation path or command guidance.
- [ ] For `auto`, preserve current behavior until Task 7 changes the default.
- [ ] Update xtask unit tests around `linux_install_build_command`, `remote_install_build_command`, and `tauri_build_command`.
- [ ] Run:

```bash
scripts/spur-cargo test -p xtask -- --nocapture
```

- [ ] Commit:

```bash
git add xtask/src/main.rs
git commit -m "feat(xtask): bd-1ox2l support notebook install channels"
```

### Task 3: Create Standalone `getspur/spur-notebook` Repository

**Task ID:** `task-3-create-standalone-repo`

**Files:**
- Read: `crates/spur-notebook/**`
- Read: `Cargo.toml`
- Create outside current repo when allowed: local checkout for `getspur/spur-notebook`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Existing uncommitted changes under `crates/spur-notebook/**` are preserved as a patch and applied to the standalone repo, or the task emits a `blocked` signal with the exact dirty files.
- [ ] The new local repo is history-preserving from `crates/spur-notebook/`.
- [ ] The GitHub remote `getspur/spur-notebook` exists by the end of the task.
- [ ] Initial standalone branch is pushed to `getspur/spur-notebook`.
- [ ] The main SPUR repo is not modified except for intentional audit artifacts if needed.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: Git history extraction, local standalone checkout creation, remote repository creation/push.
- OUT of scope: editing standalone manifests beyond path relocation needed for the first push.
- If GitHub authentication or org permission blocks repo creation, emit `blocked` with the failing command and stderr.

**Implementation:**
- [ ] Check dirty files:

```bash
git status --short crates/spur-notebook
```

- [ ] If dirty files exist, write a patch outside the source tree:

```bash
mkdir -p output/spur-notebook-split
git diff -- crates/spur-notebook > output/spur-notebook-split/dirty-spur-notebook.patch
```

- [ ] Create a history-preserving split branch:

```bash
git subtree split --prefix=crates/spur-notebook -b split/spur-notebook
```

- [ ] Create a local standalone checkout under an approved writable location such as `/private/tmp/spur-notebook`:

```bash
rm -rf /private/tmp/spur-notebook
git clone . /private/tmp/spur-notebook
git -C /private/tmp/spur-notebook checkout split/spur-notebook
```

- [ ] If `output/spur-notebook-split/dirty-spur-notebook.patch` is non-empty, rewrite the prefix and apply it in `/private/tmp/spur-notebook`.
- [ ] Verify the target remote currently fails or exists:

```bash
git ls-remote --exit-code https://github.com/getspur/spur-notebook.git HEAD
```

- [ ] If the repo does not exist, create it as private by default:

```bash
gh repo create getspur/spur-notebook --private --source /private/tmp/spur-notebook --remote origin --push
```

- [ ] If it already exists, set `origin` and push an implementation branch:

```bash
git -C /private/tmp/spur-notebook remote add origin https://github.com/getspur/spur-notebook.git
git -C /private/tmp/spur-notebook push -u origin HEAD:main
```

- [ ] Commit only any current-repo audit artifact that was intentionally created:

```bash
git add output/spur-notebook-split/dirty-spur-notebook.patch
git commit -m "chore(spur-notebook): bd-1ox2l record split dirty patch"
```

### Task 4: Standalone Manifests And Pinned Git Dependencies

**Task ID:** `task-4-standalone-manifests`

**Files:**
- Modify in `getspur/spur-notebook`: root `Cargo.toml`
- Modify in `getspur/spur-notebook`: `Cargo.toml`
- Modify in `getspur/spur-notebook`: `rest-table-gateway/Cargo.toml`
- Modify in `getspur/spur-notebook`: `jute-notebook/src-tauri/Cargo.toml`
- Modify in `getspur/spur-notebook`: `jute-notebook/package.json`
- Modify in `getspur/spur-notebook`: `jute-notebook/VENDOR.md`

**Depends on:** `task-3-create-standalone-repo`

**Acceptance Criteria:**
- [ ] Standalone repo has a root Rust workspace for notebook-owned crates.
- [ ] SPUR internal dependencies are pinned to a specific `getspur/spur` commit or tag, not a floating branch.
- [ ] Package metadata points to `https://github.com/getspur/spur-notebook`.
- [ ] `jute-notebook` frontend metadata points to the new repo.
- [ ] Standalone Rust check reaches dependency resolution.
- [ ] Frontend typecheck reaches dependency resolution.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: standalone repo manifests, metadata, and dependency pinning.
- OUT of scope: changing runtime behavior or cutting over the main SPUR workspace.
- If a private git dependency cannot be fetched, emit `blocked` with the dependency and command.

**Implementation:**
- [ ] Determine the SPUR commit to pin:

```bash
git -C /Volumes/Projects/spur rev-parse HEAD
```

- [ ] In `getspur/spur-notebook`, replace `workspace = true` dependencies on SPUR-owned crates with pinned git dependencies to that commit.
- [ ] Add a standalone root workspace that includes:

```toml
[workspace]
resolver = "2"
members = [
    ".",
    "rest-table-gateway",
    "jute-notebook/src-tauri",
]
```

- [ ] Update repository and bug metadata from `getspur/spur` to `getspur/spur-notebook`.
- [ ] Run standalone verification commands from the standalone checkout:

```bash
cargo check --workspace --no-default-features
pnpm --dir jute-notebook run typecheck
```

- [ ] Commit and push in the standalone repo:

```bash
git -C /private/tmp/spur-notebook add Cargo.toml rest-table-gateway/Cargo.toml jute-notebook/src-tauri/Cargo.toml jute-notebook/package.json jute-notebook/VENDOR.md
git -C /private/tmp/spur-notebook commit -m "feat(spur-notebook): bd-1ox2l make workspace standalone"
git -C /private/tmp/spur-notebook push
```

### Task 5: Standalone CI And Release Smoke

**Task ID:** `task-5-standalone-ci-release`

**Files:**
- Modify in `getspur/spur-notebook`: `.github/workflows/ci.yml`
- Modify in `getspur/spur-notebook`: `README.md`
- Modify in `getspur/spur-notebook`: release or packaging docs/scripts if present

**Depends on:** `task-4-standalone-manifests`

**Acceptance Criteria:**
- [ ] Standalone repo has CI for Rust check/test, frontend typecheck/test, and packaging smoke.
- [ ] CI uses the standalone repo paths, not `crates/spur-notebook/**`.
- [ ] README documents blue/green installation and SPUR integration.
- [ ] Release notes document that Phase 1 depends on pinned git dependencies back to `getspur/spur`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: standalone repo CI/docs/release smoke.
- OUT of scope: publishing binaries or cutting over SPUR defaults.
- If CI provider permissions are unavailable, commit workflow files locally and emit `blocked` only for the push.

**Implementation:**
- [ ] Add `.github/workflows/ci.yml` with jobs for:

```bash
cargo check --workspace --no-default-features
cargo test --workspace --no-default-features
pnpm --dir jute-notebook run typecheck
pnpm --dir jute-notebook test
```

- [ ] Add a packaging smoke job that verifies Tauri metadata and DuckDB extension build scripts are present; full platform packaging can remain manual in Phase 1.
- [ ] Add README sections for:
  - installing green notebook
  - using `SPUR_NOTEBOOK_CHANNEL=green`
  - rollback to blue during the transition window
- [ ] Commit and push in the standalone repo:

```bash
git -C /private/tmp/spur-notebook add .github/workflows/ci.yml README.md
git -C /private/tmp/spur-notebook commit -m "ci(spur-notebook): bd-1ox2l add standalone validation"
git -C /private/tmp/spur-notebook push
```

### Task 6: Add SPUR Green Compatibility Tests

**Task ID:** `task-6-spur-green-contract-tests`

**Files:**
- Modify: `crates/spur-core/src/notebook.rs`
- Modify: `crates/spur-tui/tests/notebook_daemon.rs`
- Modify: `docs/superpowers/specs/2026-06-16-spur-notebook-repo-split-design.md` only if implementation discovers a real contract correction

**Depends on:** `task-2-xtask-blue-green-install`, `task-5-standalone-ci-release`

**Acceptance Criteria:**
- [ ] SPUR has tests proving green channel selection uses external binary/app paths.
- [ ] SPUR has tests proving invalid or missing green gives an actionable error.
- [ ] Socket/MCP contract tests remain independent from notebook source paths.
- [ ] `scripts/spur-cargo test -p spur-core notebook -- --nocapture` passes.
- [ ] `scripts/spur-cargo test -p spur-tui --test notebook_daemon -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: compatibility tests and small resolver corrections needed to pass them.
- OUT of scope: deleting notebook source or changing standalone repo CI.
- If tests expose a protocol mismatch requiring notebook source edits, emit `scope_drift`.

**Implementation:**
- [ ] Add tests that set `SPUR_NOTEBOOK_CHANNEL=green` and use a temp executable path to simulate the external notebook.
- [ ] Add a test for missing green path that checks error text includes channel and attempted path.
- [ ] Add or update MCP stdio command tests to assert the selected binary comes from the resolver.
- [ ] Run:

```bash
scripts/spur-cargo test -p spur-core notebook -- --nocapture
scripts/spur-cargo test -p spur-tui --test notebook_daemon -- --nocapture
```

- [ ] Commit:

```bash
git add crates/spur-core/src/notebook.rs crates/spur-tui/tests/notebook_daemon.rs docs/superpowers/specs/2026-06-16-spur-notebook-repo-split-design.md
git commit -m "test(spur-core): bd-1ox2l cover green notebook channel"
```

### Task 7: Cut Over SPUR Workspace From Blue Source Ownership

**Task ID:** `task-7-cutover-spur-workspace`

**Files:**
- Modify: `Cargo.toml`
- Modify: `xtask/src/main.rs`
- Modify: `scripts/spur-pnpm`
- Modify/delete: docs that claim `crates/spur-notebook` is owned by `getspur/spur`
- Delete only after validation: `crates/spur-notebook/**`

**Depends on:** `task-6-spur-green-contract-tests`

**Acceptance Criteria:**
- [ ] `Cargo.toml` no longer lists notebook workspace members.
- [ ] `xtask install --notebook-channel green` is the documented production path.
- [ ] `scripts/spur-pnpm` no longer assumes `crates/spur-notebook/jute-notebook` exists in `getspur/spur`.
- [ ] In-tree notebook source is removed only after green repo validation is linked in the commit message or body.
- [ ] Existing non-notebook workspace checks pass.
- [ ] `scripts/spur-cargo test -p spur-core notebook -- --nocapture` passes.
- [ ] `scripts/spur-cargo test -p xtask -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: final source ownership cutover in `getspur/spur`.
- OUT of scope: changing standalone repo internals.
- If `git status --short crates/spur-notebook` shows uncommitted user changes not already preserved in Task 3, emit `blocked` and do not delete the subtree.

**Implementation:**
- [ ] Confirm green validation artifacts exist:

```bash
git -C /private/tmp/spur-notebook log --oneline -5
git ls-remote --exit-code https://github.com/getspur/spur-notebook.git HEAD
```

- [ ] Confirm dirty notebook changes have been preserved:

```bash
git status --short crates/spur-notebook
test -f output/spur-notebook-split/dirty-spur-notebook.patch || true
```

- [ ] Remove notebook members from root `Cargo.toml`.
- [ ] Update `xtask` default docs/help to point production users at green.
- [ ] Replace or remove `scripts/spur-pnpm`; if kept, make it print an actionable message that notebook frontend commands now live in `getspur/spur-notebook`.
- [ ] Delete `crates/spur-notebook/**` only after the preservation check passes.
- [ ] Run:

```bash
scripts/spur-cargo test -p spur-core notebook -- --nocapture
scripts/spur-cargo test -p xtask -- --nocapture
```

- [ ] Commit:

```bash
git add Cargo.toml xtask/src/main.rs scripts/spur-pnpm docs crates/spur-notebook
git commit -m "refactor(spur-notebook): bd-1ox2l cut over to green repo"
```

### Task 8: Add Phase 2 Shared-Crate Extraction Plan

**Task ID:** `task-8-phase2-shared-crate-plan`

**Files:**
- Create: `docs/superpowers/plans/2026-06-16-spur-notebook-shared-crates-phase2.md`
- Modify in `getspur/spur-notebook`: README or tracking issue link if available

**Depends on:** `task-7-cutover-spur-workspace`

**Acceptance Criteria:**
- [ ] Phase 2 identifies every pinned git dependency from `getspur/spur-notebook` back to `getspur/spur`.
- [ ] Each dependency has a target disposition: publish crate, extract shared protocol crate, or remove dependency.
- [ ] Plan includes compatibility-version tests for SPUR and notebook.
- [ ] The plan explicitly keeps Phase 2 separate from the Phase 1 blue/green cutover.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: Phase 2 planning artifact and links from repo docs.
- OUT of scope: actually publishing crates or changing Phase 1 cutover code.
- If Phase 1 cutover is not complete, emit `blocked`.

**Implementation:**
- [ ] Inspect standalone manifests:

```bash
rg -n "git = .*getspur/spur|spur-" /private/tmp/spur-notebook Cargo.toml
```

- [ ] Write `docs/superpowers/plans/2026-06-16-spur-notebook-shared-crates-phase2.md` with a dependency table and migration order.
- [ ] Add a link from standalone README if the standalone repo is writable.
- [ ] Commit:

```bash
git add docs/superpowers/plans/2026-06-16-spur-notebook-shared-crates-phase2.md
git commit -m "docs(spur-notebook): bd-1ox2l plan shared crate phase two"
```

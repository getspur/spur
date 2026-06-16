# Spur Notebook Standalone Repository Design

**Design epic:** `bd-1ox2l`
**Decision date:** 2026-06-16
**Target repository:** `getspur/spur-notebook`

## Goal

Separate `crates/spur-notebook/` from `getspur/spur` into `getspur/spur-notebook`
as the source of truth for the notebook product, while keeping SPUR able to
launch or communicate with an installed notebook product.

The first production milestone optimizes for a working repository split. The
second milestone hardens the dependency boundary by moving shared SPUR APIs to
published or otherwise versioned crates.

## Current Shape

`crates/spur-notebook/` is a product subtree, not a leaf Rust crate. It contains:

- The `spur-notebook` Rust binary/library.
- The nested `jute-notebook` React/Tauri app.
- The nested `jute` Rust crate at `jute-notebook/src-tauri`.
- `rest-table-gateway` and the separate `rest-table-gateway-ext` DuckDB
  extension workspace.
- Notebook assets, open-design libraries, Tauri resources, tests, and docs.

The main SPUR workspace currently treats these as in-tree members:

- `crates/spur-notebook`
- `crates/spur-notebook/rest-table-gateway`
- `crates/spur-notebook/jute-notebook/src-tauri`

`xtask` also builds and installs notebook artifacts from these in-tree paths.

## Chosen Approach

Use the standalone-source-of-truth model.

`getspur/spur-notebook` owns notebook source, tests, CI, and release artifacts.
`getspur/spur` no longer owns notebook source after the extraction. The main
SPUR repository keeps only the integration surface required to discover, launch,
or communicate with the external notebook product.

## Blue/Green Migration Requirement

The repo split must be blue/green so the production cutover can be validated and
rolled back without blocking the SPUR product.

Definitions:

- **Blue:** the current in-tree notebook build and install behavior from
  `getspur/spur`.
- **Green:** the standalone notebook product built and released from
  `getspur/spur-notebook`.

Rules:

- SPUR must support a transition window where blue and green can both be
  exercised by developers or release validation.
- Green is opt-in first. A config flag or environment override selects the
  standalone notebook before it becomes the default.
- The SPUR launcher must report which channel it selected and why.
- If green is selected but unavailable, SPUR must fail with an actionable
  install/version message. During the transition window, fallback to blue is
  allowed only when blue still exists in that branch or release line.
- Blue source removal from `getspur/spur` happens only after green passes
  launcher, socket, MCP, and packaging parity checks.
- Rollback means switching the launcher back to blue or to the previously
  released notebook binary, not reintroducing notebook source into `getspur/spur`.

## Phase 1: Fast Standalone Split

Create `getspur/spur-notebook` from the existing subtree history and make it
build independently.

Repository contents:

- Move the current `crates/spur-notebook/` contents to the new repo root or to a
  shallow layout that keeps `spur-notebook`, `jute-notebook`,
  `rest-table-gateway`, and `rest-table-gateway-ext` easy to build together.
- Add a root `Cargo.toml` workspace for the notebook-owned crates.
- Add frontend package workflow support for `jute-notebook`.
- Add CI for Rust checks/tests, frontend typecheck/tests, Tauri build smoke, and
  DuckDB extension packaging smoke.
- Update package metadata, repository URLs, issue URLs, and vendor notes to point
  at `getspur/spur-notebook`.

Dependency model:

- Initially use pinned git dependencies back to `getspur/spur` for SPUR internal
  crates such as `spur-acp`, `spur-core`, and `spur-graph`.
- Pin by commit or tag, not by floating branch.
- Keep the set of git dependencies explicit in the new repo root so the future
  shared-crate extraction is visible.

Changes in `getspur/spur`:

- Remove notebook crates from workspace membership after green has a working
  build.
- Remove in-tree notebook build and packaging ownership from `xtask`.
- Keep launcher/integration code that can find and start an externally installed
  `spur-notebook`.
- Keep protocol compatibility tests and docs for the notebook socket/MCP
  boundary.

## Phase 2: Production Boundary Hardening

Replace git dependencies on `getspur/spur` with versioned shared crates.

Candidates:

- ACP/session protocol types used by notebook.
- Notebook-facing graph/context APIs.
- Shared app/protocol contracts needed by the launcher, socket, and MCP tools.

The phase 2 output is a release discipline where `getspur/spur-notebook` can
declare compatible SPUR API versions instead of depending on arbitrary SPUR
commits.

## Integration Contract

SPUR integration after extraction is process/protocol-based:

- SPUR locates the installed notebook binary or app bundle.
- SPUR launches notebook with the expected socket/control arguments.
- SPUR talks over the existing daemon socket and MCP contracts.
- Notebook reports its product version, compatible protocol version, selected
  channel, and source repository metadata.

The main SPUR repo must not depend on notebook source paths after phase 1.

## Error Handling

Launcher errors must distinguish:

- Notebook is not installed.
- Installed notebook is too old for the SPUR protocol contract.
- Notebook failed to launch.
- Socket connection timed out.
- Green channel was requested but unavailable.

Each error should include the selected channel, attempted path, expected version
or protocol range, and the command or documentation path needed to install the
correct notebook product.

## Testing

Phase 1 acceptance tests:

- `getspur/spur-notebook` builds the notebook Rust crates.
- `getspur/spur-notebook` typechecks and tests the frontend.
- `getspur/spur-notebook` builds or smoke-tests the Tauri bundle path.
- `getspur/spur-notebook` builds or smoke-tests the DuckDB extension path.
- `getspur/spur` no longer has notebook workspace members after cutover.
- `getspur/spur` launcher can select green and connect to its socket/MCP
  contract.
- During the blue/green transition branch, blue and green launcher selection are
  both testable.

Phase 2 acceptance tests:

- Notebook no longer depends on private SPUR workspace crates by git.
- Shared protocol crates have versioned compatibility tests.
- SPUR and notebook reject incompatible protocol versions with clear errors.

## Task Boundaries For Implementation Planning

Implementation should be split into focused work units:

- History-preserving repository extraction.
- Standalone notebook workspace manifests and metadata.
- Notebook CI and release workflow.
- SPUR blue/green launcher selection.
- SPUR workspace and `xtask` cleanup.
- Compatibility and protocol tests.
- Phase 2 shared-crate extraction plan.

Each task should touch a narrow file set. If a task needs to change both repos,
the implementation plan should make the ordering and rollback point explicit.

## Out Of Scope

- Renaming the user-facing app from `Jute` or changing product branding.
- Rewriting notebook runtime architecture.
- Removing SPUR launcher integration.
- Publishing shared crates during the first extraction milestone.

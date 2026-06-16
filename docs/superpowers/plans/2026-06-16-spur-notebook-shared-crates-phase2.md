# Spur Notebook Shared Crates Phase 2 Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-16-spur-notebook-repo-split-design.md`
**Design epic:** `bd-1ox2l` (closed)

**Goal:** Replace `getspur/spur-notebook` pinned git dependencies back to
`getspur/spur` with versioned shared crates and compatibility gates.

**Architecture:** Phase 2 is a dependency-boundary hardening phase, not a
Phase 1 cutover change. The notebook keeps using the green standalone repository
and migrates from commit-pinned SPUR workspace crates to small semver contracts:
notebook launch/socket contracts, ACP agent protocol/client contracts, and graph
fact extraction contracts.

**Tech Stack:** Rust workspace crates, Cargo semver dependencies, serde fixture
tests, SPUR launcher/socket protocol tests, standalone `getspur/spur-notebook`
Cargo manifests.

---

## Phase Boundary

Phase 1 is complete before this plan starts:

- `getspur/spur` cutover commit: `5bb38d18e refactor(spur-notebook): bd-1ox2l cut over to green repo`
- The current SPUR worktree no longer contains `crates/spur-notebook/`.
- `scripts/spur-pnpm` is a compatibility wrapper that forwards to
  `SPUR_NOTEBOOK_REPO=/path/to/spur-notebook`.
- `/private/tmp/spur-notebook` is the local standalone checkout used for the
  dependency inventory below.

This plan does not publish crates and does not change Phase 1 launcher, xtask,
workspace, or blue/green cutover behavior. Publishing is a later release action
after these extraction tasks and compatibility tests pass.

## Pinned Dependency Inventory

Inventory source:

```bash
rg -n "getspur/spur|git\\s*=|rev\\s*=" /private/tmp/spur-notebook -g 'Cargo.toml' -g 'Cargo.lock'
```

Direct pinned declarations in `/private/tmp/spur-notebook/Cargo.toml`:

| Direct crate | Pin | Current notebook use | Disposition |
|---|---|---|---|
| `spur-acp` | `https://github.com/getspur/spur.git`, rev `c9eec99f9c874a151b4ec49c832caf1ae4b1eccb` | `AgentConnection`, `AgentConfig`, `SpurConfig`, `PermissionRequest`, `PermissionResponse`, `AgentHealth`, `TransportKind` in sidebar chat, DAG AI, and Jute chat state. | Extract/publish a versioned ACP API surface. Split or slim the crate so notebook depends only on protocol/config/client traits and not SPUR project-management internals. |
| `spur-core` | same rev | `notebook_binary_path()` and `control_socket_path()` in the standalone binary. | Remove the direct dependency. Move launch/socket constants and compatibility structs into a shared notebook contract crate consumed by both SPUR and notebook. |
| `spur-graph` | same rev | `GraphFacts`, `GraphNode`, `GraphEdge`, `NodeId`, `NodeKind`, `RelationKind`, and `extract_notebook_facts()` for notebook symbol index and MCP symbol tools. | Extract/publish versioned graph fact schema plus notebook extractor contract. Keep full SPUR graph indexing/search internals out of notebook. |

All resolved SPUR git packages in `/private/tmp/spur-notebook/Cargo.lock`:

| Resolved crate | Direct? | Pulled by | Migration disposition |
|---|---:|---|---|
| `spur-acp` | yes | root `Cargo.toml`, `jute-notebook/src-tauri/Cargo.toml` | Replace with versioned ACP API crate after config/client surface is isolated. |
| `spur-core` | yes | root `Cargo.toml` | Remove by moving notebook launch/socket contract into the shared notebook contract crate. |
| `spur-graph` | yes | root `Cargo.toml`; also by `spur-analyst`/`spur-mcp` transitively | Replace with versioned graph fact/extractor crate; full `spur-graph` remains SPUR-owned. |
| `spur-analyst` | no | `spur-mcp` transitively through `spur-core` | Remove. Notebook semantic search may consume analyst database files, but it should depend on documented schema fixtures, not the `spur-analyst` crate. |
| `spur-blob-store` | no | `spur-core`, `spur-mcp`, `spur-worktree` | Remove transitively when `spur-core`/`spur-mcp` are gone from notebook's graph. |
| `spur-cost` | no | `spur-core` | Remove transitively; notebook does not import cost ledger APIs. |
| `spur-license` | no | `spur-core`, `spur-mcp` | Remove transitively; notebook does not import license policy APIs. |
| `spur-mcp` | no | `spur-core` | Remove transitively. Notebook owns its MCP server implementation and should share only protocol/schema crates where needed. |
| `spur-pm` | no | `spur-acp`, `spur-core`, `spur-mcp` | Remove transitively. Any ACP config that currently drags project-management types must be split behind a SPUR-only feature or moved out of the notebook-facing API. |
| `spur-worktree` | no | `spur-core`, `spur-mcp` | Remove transitively; notebook should not depend on SPUR worktree orchestration. |

After Phase 2, this command must return no rows:

```bash
rg -n "github.com/getspur/spur.git" /private/tmp/spur-notebook -g 'Cargo.toml' -g 'Cargo.lock'
```

## File Structure Mapping

- `getspur/spur`:
  - Create shared crate for notebook launch/socket/version contracts.
  - Split or publish ACP protocol/client APIs without SPUR PM transitives.
  - Split graph facts/notebook extractor from full graph indexing internals.
  - Add compatibility tests and a guard against reintroducing notebook source
    ownership.
- `getspur/spur-notebook`:
  - Replace git dependencies in root `Cargo.toml`.
  - Keep `jute-notebook/src-tauri/Cargo.toml` on workspace dependencies only.
  - Add a CI guard that rejects `git+https://github.com/getspur/spur.git`.
  - Keep default CI scoped to Rust check/clippy and frontend typecheck.
  - Run full Rust/unit/compatibility suites only in an explicit remote preflight
    or release-gate workflow.
  - Keep README release notes linked to this Phase 2 plan.

## Verification And Disk-Pressure Policy

Phase 2 must preserve the disk-pressure policy established during the split:
compile-heavy checks do not run as local bare `cargo` commands from SPUR
worktrees, and full standalone test suites are not part of the default
standalone CI template.

SPUR-side Rust verification uses the GCP-backed wrapper:

```bash
SPUR_REMOTE=1 scripts/spur-cargo check --workspace
SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-core notebook -- --nocapture
```

Standalone frontend verification from this repo uses the post-split forwarding
wrapper:

```bash
SPUR_NOTEBOOK_REPO=/private/tmp/spur-notebook scripts/spur-pnpm install --frozen-lockfile
SPUR_NOTEBOOK_REPO=/private/tmp/spur-notebook scripts/spur-pnpm run typecheck
```

Standalone Rust `check`/`clippy` remain the default CI Rust scope. Full
standalone Rust unit tests, graph parity tests, and cross-repo compatibility
suites are release gates: run them through the Phase 2 GCP/remote preflight
workflow introduced in Task 1, not as local `cargo test --workspace` steps and
not as default CI.

## Compatibility Contract

The shared crates must carry explicit version boundaries:

- Notebook launch/socket contract exposes a `NOTEBOOK_PROTOCOL_VERSION`, the
  `SPUR_NOTEBOOK_CHANNEL` and `SPUR_NOTEBOOK_BIN` env names, control socket path
  derivation, and the daemon version handshake payload.
- ACP API crate exposes serde-stable request/response/config fixtures for the
  notebook-facing types used today.
- Graph fact crate exposes serde-stable fixtures for `GraphFacts`, `GraphNode`,
  `GraphEdge`, and relation/node kind enums, plus notebook extraction parity
  fixtures.

Both repos must reject incompatible protocol versions with clear errors that
include the installed notebook version, SPUR expected range, selected channel,
and attempted path.

## Dependency DAG

```text
task-1-freeze-inventory-and-guards
  -> task-2-notebook-contract-crate
  -> task-5-standalone-manifest-migration
  -> task-6-phase2-release-gates

task-1-freeze-inventory-and-guards
  -> task-3-acp-api-extraction
  -> task-5-standalone-manifest-migration

task-1-freeze-inventory-and-guards
  -> task-4-graph-facts-extraction
  -> task-5-standalone-manifest-migration
```

### Task 1: Freeze Dependency Inventory And Guards

**Task ID:** `task-1-freeze-inventory-and-guards`

**Files:**
- Modify in `getspur/spur-notebook`: `Cargo.toml`
- Modify in `getspur/spur-notebook`: `.github/workflows/ci.yml`
- Modify in `getspur/spur-notebook`: `README.md`
- Create/modify in `getspur/spur-notebook`: Phase 2 GCP/remote preflight
  workflow for full Rust/unit/compatibility suites.

**Depends on:** none

**Acceptance Criteria:**
- [ ] Current direct pins are documented as `spur-acp`, `spur-core`, and `spur-graph`.
- [ ] Current lockfile SPUR git packages are documented as the ten crates listed in this plan.
- [ ] CI has a non-blocking Phase 2 audit step that prints any `github.com/getspur/spur.git` source.
- [ ] Phase 1 CI still allows the existing pins until Task 5 removes them.
- [ ] Default standalone CI stays scoped to Rust check/clippy and frontend typecheck.
- [ ] Full standalone Rust/unit/compatibility suites have an explicit
      GCP/remote preflight or release-gate entry point, separate from default CI.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: docs and audit-only guard output.
- OUT of scope: changing dependency declarations or publishing crates.
- If the standalone checkout has uncommitted user changes, emit `blocked`.

**Implementation:**
- [ ] Add a CI step that runs:

```bash
rg -n "github.com/getspur/spur.git" Cargo.toml Cargo.lock || true
```

- [ ] Add a README note that this is audit-only until Phase 2 migration.
- [ ] Document that full standalone test suites are remote preflight/release
      gates, not default CI.
- [ ] Commit in `getspur/spur-notebook`:

```bash
git add Cargo.toml .github/workflows/ci.yml README.md
git commit -m "docs(spur-notebook): bd-1ox2l freeze shared crate inventory"
```

### Task 2: Extract Notebook Launch Contract

**Task ID:** `task-2-notebook-contract-crate`

**Files:**
- Create in `getspur/spur`: shared notebook contract crate.
- Modify in `getspur/spur`: `crates/spur-core/src/notebook.rs`
- Modify in `getspur/spur`: `crates/spur-core` notebook resolver tests.
- Modify in `getspur/spur-notebook`: `src/main.rs`

**Depends on:** `task-1-freeze-inventory-and-guards`

**Acceptance Criteria:**
- [ ] Standalone notebook no longer imports `spur_core`.
- [ ] SPUR and notebook share one implementation for control socket path derivation.
- [ ] Version handshake structs serialize with stable field names.
- [ ] SPUR launcher rejects an incompatible notebook protocol version with selected channel and attempted path in the error.
- [ ] Notebook reports its product version, protocol version, and source repository metadata.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: launch/socket/version contract only.
- OUT of scope: changing blue/green resolver order, xtask install behavior, or removing rollback paths.

**Implementation:**
- [ ] Move `control_socket_path` semantics into the shared contract crate.
- [ ] Add serde fixtures for the daemon handshake payload.
- [ ] Update SPUR resolver tests and notebook daemon startup tests to use the shared crate.
- [ ] Run:

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-core notebook -- --nocapture
gh -R getspur/spur-notebook workflow run phase2-remote-preflight.yml -f suite=notebook-contract
```

### Task 3: Extract ACP API Surface

**Task ID:** `task-3-acp-api-extraction`

**Files:**
- Modify/create in `getspur/spur`: ACP shared API crate or slimmed `spur-acp`.
- Modify in `getspur/spur`: ACP serde round-trip tests.
- Modify in `getspur/spur-notebook`: root `Cargo.toml`
- Modify in `getspur/spur-notebook`: `jute-notebook/src-tauri/Cargo.toml`

**Depends on:** `task-1-freeze-inventory-and-guards`

**Acceptance Criteria:**
- [ ] Notebook-facing ACP types do not depend on `spur-pm`.
- [ ] Notebook-facing ACP client traits do not depend on `spur-core`, `spur-mcp`, or `spur-worktree`.
- [ ] `AgentConfig`, `SpurConfig`, `AgentConnection`, `PermissionRequest`, `PermissionResponse`, `AgentHealth`, and `TransportKind` have serde/API compatibility tests.
- [ ] Notebook compiles against the versioned ACP API dependency without a git pin to `getspur/spur`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: notebook-facing ACP protocol/config/client surface.
- OUT of scope: SPUR project-management adapters, beads integration, or session ledger behavior.

**Implementation:**
- [ ] Split PM-specific config from notebook-facing agent config, or gate it behind a SPUR-only feature disabled for notebook.
- [ ] Add fixture tests for each notebook-used ACP type.
- [ ] Update notebook manifests to use the versioned ACP API crate.
- [ ] Run:

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-acp -- --nocapture
gh -R getspur/spur-notebook workflow run phase2-remote-preflight.yml -f suite=acp-api
```

### Task 4: Extract Graph Facts And Notebook Extractor

**Task ID:** `task-4-graph-facts-extraction`

**Files:**
- Modify/create in `getspur/spur`: graph fact schema/extractor crate.
- Modify in `getspur/spur`: `crates/spur-graph/src/extract/mod.rs`
- Modify in `getspur/spur-notebook`: `src/context/catalog.rs`
- Modify in `getspur/spur-notebook`: `src/context/symbol_index.rs`
- Modify in `getspur/spur-notebook`: `src/mcp/tools/notebook_symbol_refs.rs`
- Modify in `getspur/spur-notebook`: `src/mcp/tools/notebook_symbol_search.rs`

**Depends on:** `task-1-freeze-inventory-and-guards`

**Acceptance Criteria:**
- [ ] Notebook no longer imports full `spur_graph`.
- [ ] Shared graph crate exposes `GraphFacts`, `GraphNode`, `GraphEdge`, `NodeId`, `NodeKind`, and `RelationKind`.
- [ ] Notebook extraction parity fixtures prove `extract_notebook_facts()` returns the same graph facts before and after extraction.
- [ ] Full SPUR indexing/search internals remain out of the notebook dependency graph.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: graph fact schema and notebook file extraction.
- OUT of scope: Lance/LanceDB index ownership, analyst SQL engine, or general SPUR graph search refactors.

**Implementation:**
- [ ] Move shared fact types and notebook extractor into a semver crate.
- [ ] Re-export from `spur-graph` temporarily if needed for SPUR internal call sites.
- [ ] Update notebook symbol index/tool code to use the shared crate.
- [ ] Run:

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph notebook -- --nocapture
gh -R getspur/spur-notebook workflow run phase2-remote-preflight.yml -f suite=graph-facts
```

### Task 5: Migrate Standalone Manifests Off Git Pins

**Task ID:** `task-5-standalone-manifest-migration`

**Files:**
- Modify in `getspur/spur-notebook`: `Cargo.toml`
- Modify in `getspur/spur-notebook`: `Cargo.lock`
- Modify in `getspur/spur-notebook`: `.github/workflows/ci.yml`
- Modify in `getspur/spur-notebook`: `README.md`

**Depends on:** `task-2-notebook-contract-crate`, `task-3-acp-api-extraction`, `task-4-graph-facts-extraction`

**Acceptance Criteria:**
- [ ] `rg -n "github.com/getspur/spur.git" Cargo.toml Cargo.lock` returns no rows.
- [ ] The Phase 2 dependency-tree preflight shows no `spur-core`, `spur-mcp`, `spur-pm`, `spur-worktree`, `spur-cost`, `spur-license`, `spur-blob-store`, or `spur-analyst`.
- [ ] Standalone Rust check/clippy and frontend typecheck pass without private `getspur/spur` read access.
- [ ] Full standalone unit/compatibility suites pass in the explicit remote
      preflight before release, not in default CI.
- [ ] CI removes the `SPUR_REPO_READ_TOKEN` requirement after the git pins are gone.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: standalone manifest migration and CI guard flip from audit to enforcement.
- OUT of scope: crate publishing itself; use already versioned crate artifacts from Tasks 2-4.

**Implementation:**
- [ ] Replace `spur-acp`, `spur-core`, and `spur-graph` git dependencies with versioned shared crates.
- [ ] Regenerate `Cargo.lock`.
- [ ] Change CI guard to fail on any `github.com/getspur/spur.git` row.
- [ ] Push the branch and verify default CI's Rust check/clippy and frontend
      typecheck jobs pass; do not add full unit tests to default CI.
- [ ] Run:

```bash
rg -n "github.com/getspur/spur.git" Cargo.toml Cargo.lock
gh -R getspur/spur-notebook workflow run phase2-remote-preflight.yml -f suite=manifest-migration
```

### Task 6: Add Phase 2 Release Gates

**Task ID:** `task-6-phase2-release-gates`

**Files:**
- Modify in `getspur/spur`: launcher/protocol compatibility tests.
- Modify in `getspur/spur-notebook`: daemon compatibility tests.
- Modify in `getspur/spur-notebook`: `README.md`

**Depends on:** `task-5-standalone-manifest-migration`

**Acceptance Criteria:**
- [ ] SPUR rejects too-old and too-new notebook protocol versions with clear errors.
- [ ] Notebook rejects unsupported SPUR API ranges with clear errors.
- [ ] README documents the minimum compatible SPUR version and shared crate versions.
- [ ] CI in both repos includes scoped fixture/protocol-version checks that do
      not expand default standalone CI into the full unit suite.
- [ ] The full cross-repo compatibility suite is an explicit remote
      release-gate preflight.
- [ ] No Phase 1 blue source or cutover behavior is reintroduced.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: release-readiness tests and docs.
- OUT of scope: publishing binaries, publishing crates, or changing the blue/green launcher default.

**Implementation:**
- [ ] Add SPUR-side tests for incompatible notebook protocol handshake payloads.
- [ ] Add notebook-side tests for incompatible SPUR API range payloads.
- [ ] Document the compatibility matrix.
- [ ] Run:

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-core notebook -- --nocapture
gh -R getspur/spur-notebook workflow run phase2-remote-preflight.yml -f suite=compatibility
```

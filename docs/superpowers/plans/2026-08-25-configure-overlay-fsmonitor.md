# Experimental Overlay Fsmonitor Configure Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-25-configure-overlay-fsmonitor.md`
**Formal @spec cells:** none
**Design epic:** `bd-1vt5` (closed)

**Goal:** Add a default-Off, restart-applied experimental Auto fsmonitor option to `/configure graph` and seed it into new brain graph MCP servers.

**Architecture:** `spur-acp` owns the serialized enum and typed patch. The TUI edits it through the existing persist-then-apply flow. `spur-core` copies the confirmed startup config into `GraphMcpDeps`, and `spur-graph` uses that immutable dependency to decide whether to probe Git or stay exact; requests never read config files.

**Tech Stack:** Rust 2021, serde/TOML, ratatui, existing graph MCP and Git fsmonitor probe, `scripts/spur-cargo`.

**PRE SOLVE:** target `sol_bab0ccf390dd4ebd`; current gap `sol_bfef0d6ec3b34286`; forced-native rejection `sol_bfff64fff6294637`.

---

### Task 1: Define the persisted Off/Auto contract

**Task ID:** `config-contract`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `OverlayFsmonitorMode` serializes as `off`/`auto`, defaults to Off, and supports display labels used by the TUI.
- [ ] `GraphConfig.overlay_fsmonitor` is serde-defaulted and omitted when Off.
- [ ] `ConfigPatch::GraphOverlayFsmonitor` reports section `graph` and applies the value.
- [ ] Focused `spur-acp` tests pass with no compilation warnings.

**Suggested Worker:** codex, model `gpt-5.6-sol`, effort `xhigh` (user-directed)

**Scope Boundary:**
- IN scope: the one config module and its inline tests.
- OUT of scope: TUI rendering, MCP runtime routing, standalone/worker servers.
- Any additional file requires a `scope_drift` signal.

**Implementation:**
- [ ] **Step 1: Write failing tests** that parse missing/`off`/`auto`, assert default serialization omission, and apply `ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)`.

```rust
#[test]
fn graph_overlay_fsmonitor_defaults_off_and_patch_applies_auto() {
    let mut cfg = SpurConfig::default();
    assert_eq!(cfg.graph.overlay_fsmonitor, OverlayFsmonitorMode::Off);
    ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
        .apply(&mut cfg)
        .unwrap();
    assert_eq!(cfg.graph.overlay_fsmonitor, OverlayFsmonitorMode::Auto);
}
```

- [ ] **Step 2: Run RED** with `scripts/spur-cargo test -p spur-acp graph_overlay_fsmonitor -- --nocapture`; record the expected missing-type/variant failure and commit only the tests as `test(spur-acp): config-contract define overlay fsmonitor config contract`.
- [ ] **Step 3: Implement minimal GREEN** with the enum, graph field, default check, patch variant, `section_id`, and `apply` arm.
- [ ] **Step 4: Run GREEN** with the same focused command plus `scripts/spur-cargo test -p spur-acp config -- --nocapture`.
- [ ] **Step 5: Commit** as `feat(spur-acp): config-contract add overlay fsmonitor mode`.

### Task 2: Gate graph overlay status with immutable runtime policy

**Task ID:** `graph-runtime`

**Files:**
- Modify: `crates/spur-graph/src/mcp/mod.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `GraphMcpDeps` has a default-false `overlay_fsmonitor_auto` policy bit.
- [ ] Off passes fail-closed capabilities and never probes/native-routes.
- [ ] Auto calls the existing Git daemon capability probe for the request worktree and feeds the result into the existing snapshot/status machinery.
- [ ] Unsupported/unhealthy probe results and optimized command/parse errors preserve exact fallback.
- [ ] The existing overlay validation, retry, and cache identity semantics remain unchanged.

**Suggested Worker:** codex, model `gpt-5.6-sol`, effort `xhigh` (user-directed)

**Scope Boundary:**
- IN scope: graph MCP dependency/policy threading and inline tests in `mcp/mod.rs`.
- OUT of scope: changes to the already-reviewed status parser and fsmonitor route in `git.rs`; release-default changes; benchmark thresholds.
- Any additional file requires a `scope_drift` signal.

**Implementation:**
- [ ] **Step 1: Write failing tests** proving default deps are Off, Off returns `ReleaseDisabled`, Auto delegates to the probe-derived capability path, and the overlay helper receives the immutable policy.

```rust
#[test]
fn graph_mcp_deps_default_keeps_overlay_fsmonitor_off() {
    assert!(!GraphMcpDeps::default().overlay_fsmonitor_auto);
}
```

- [ ] **Step 2: Run RED** with `scripts/spur-cargo test -p spur-graph graph_mcp_deps_default_keeps_overlay_fsmonitor_off -- --nocapture`; commit the failing tests as `test(spur-graph): graph-runtime define runtime fsmonitor opt-in`.
- [ ] **Step 3: Implement minimal GREEN** by carrying the policy from `GraphMcpModule` through `code_search_response`/refresh into overlay construction and deriving capabilities once per snapshot attempt. Auto is an operator opt-in for local repositories; Git daemon health remains the compatibility check.
- [ ] **Step 4: Run GREEN** with focused fsmonitor/overlay/cache tests and `scripts/spur-cargo test -p spur-graph --lib`.
- [ ] **Step 5: Commit** as `feat(spur-graph): graph-runtime probe fsmonitor for Auto overlays`.

### Task 3: Seed the policy into new brain MCP servers

**Task ID:** `startup-wiring`

**Files:**
- Modify: `crates/spur-core/src/server/mod.rs`
- Modify: `crates/spur-core/src/orchestrator/support.rs`

**Depends on:** `config-contract`, `graph-runtime`

**Acceptance Criteria:**
- [ ] `McpCallbackServer` exposes a startup setter that only mutates `graph_mcp_deps` before server start.
- [ ] `Orchestrator::apply_mcp_server_settings` maps Off→false and Auto→true for all three brain-session initialization paths it already owns.
- [ ] Applying a `/configure` patch does not live-toggle an existing server; the UI/runtime message remains restart-required.
- [ ] Core tests prove Off and Auto seeding without starting an external Git daemon.

**Suggested Worker:** codex, model `gpt-5.6-sol`, effort `xhigh` (user-directed)

**Scope Boundary:**
- IN scope: the callback server setter, orchestrator startup mapping, and inline tests in the two named files.
- OUT of scope: standalone CLI registry composition and delegated-worker MCP composition.
- Any additional file requires a `scope_drift` signal.

**Implementation:**
- [ ] **Step 1: Write failing tests** around a test-visible accessor/setter and `apply_mcp_server_settings` mapping `OverlayFsmonitorMode::Auto` to true.
- [ ] **Step 2: Run RED** with `scripts/spur-cargo test -p spur-core overlay_fsmonitor -- --nocapture`; commit tests as `test(spur-core): startup-wiring define fsmonitor startup wiring`.
- [ ] **Step 3: Implement minimal GREEN** with `set_overlay_fsmonitor_auto(bool)` and the configuration match in `apply_mcp_server_settings`.
- [ ] **Step 4: Run GREEN** with the focused test and relevant callback-server/orchestrator suites.
- [ ] **Step 5: Commit** as `feat(spur-core): startup-wiring seed graph fsmonitor policy at startup`.

### Task 4: Add the `/configure graph` Off/Auto control

**Task ID:** `configure-ui`

**Files:**
- Modify: `crates/spur-tui/src/views/settings_graph.rs`
- Modify: `crates/spur-tui/src/views/agent_config_browser.rs`
- Modify: `crates/spur-tui/src/app/action_routing/nav.rs`

**Depends on:** `config-contract`

**Acceptance Criteria:**
- [ ] The graph pane has separate keyboard-selectable rows for embedding model and overlay fsmonitor mode.
- [ ] Saving the fsmonitor row emits `ConfigPatch::GraphOverlayFsmonitor` without changing the embedding model.
- [ ] Opening/reopening `/configure graph` reflects the confirmed `App.config.graph.overlay_fsmonitor` value.
- [ ] Copy states `Auto (experimental)`, local repositories, exact fallback, and restart requirement.
- [ ] Existing embedding-model behavior and save acknowledgement semantics remain green.

**Suggested Worker:** codex, model `gpt-5.6-sol`, effort `xhigh` (user-directed)

**Scope Boundary:**
- IN scope: graph-pane interaction/rendering, browser config refresh seam, and configure navigation wiring in the three named files.
- OUT of scope: changing global key bindings, live runtime mutation, unrelated settings panes.
- Any additional file requires a `scope_drift` signal.

**Implementation:**
- [ ] **Step 1: Write failing tests** for initial Auto selection, Up/Down row focus, Left/Right/Enter mode cycling, and `s` emitting the fsmonitor patch.

```rust
#[test]
fn graph_pane_saves_auto_without_forcing_native() {
    let mut pane = GraphPane::new(None, OverlayFsmonitorMode::Auto);
    pane.select_overlay_fsmonitor_for_test();
    assert!(matches!(
        pane.handle_key(key('s')),
        Some(Action::ConfigSaveRequested {
            patch: ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
        })
    ));
}
```

- [ ] **Step 2: Run RED** with `scripts/spur-cargo test -p spur-tui graph_pane -- --nocapture`; commit tests as `test(spur-tui): configure-ui define configure fsmonitor control`.
- [ ] **Step 3: Implement minimal GREEN** using an explicit selected-row enum/index, mode cycling, patch selection, explanatory copy, and `view.set_graph_config(&self.config.graph)` on configure open/refresh.
- [ ] **Step 4: Run GREEN** with focused graph-pane/browser/session-config tests and `scripts/spur-cargo test -p spur-tui --lib`.
- [ ] **Step 5: Commit** as `feat(spur-tui): configure-ui expose experimental fsmonitor Auto mode`.

---

## DAG

```text
config-contract ─┬─> startup-wiring
                 └─> configure-ui
graph-runtime ──────> startup-wiring
```

## Brain verification and POST SOLVE

After all four tasks are independently reviewed:

1. Run `scripts/spur-cargo fmt --all -- --check`.
2. Run focused suites for `spur-acp`, `spur-graph`, `spur-core`, and `spur-tui` using `scripts/spur-cargo`.
3. Run `scripts/spur-cargo check --workspace` and `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`.
4. Reload PRE artifacts by ID and rerun the same configuration rules with all six feature components selected, Off→exact and Auto→probe/fallback allowed, and Auto→native rejected.
5. Request an independent read-only review against this plan before merging the execution branch into local `main`.

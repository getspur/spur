# Notebook Port Integration — Explicit Per-Session Provisioning Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-02-notebook-port-integration-design.ipynb`

**Goal:** Stop prepending the port helper to every cell; instead ship each language's helper as a checked-in asset, bind the per-notebook root via a kernel-launch env var, and inject the helper once per kernel session.

**Architecture:** Adopt decision **B + env binding + asset files** from the spec. The `SPUR_NOTEBOOK_PORT_ROOT` env var is injected into the kernel process at spawn; the static, notebook-independent helper asset is run once via a silent `execute_request` right after a kernel becomes live (and on restart); per-cell wrapping is deleted so cells run verbatim.

**Tech Stack:** Rust (Tauri backend, `crates/spur-notebook/jute-notebook/src-tauri`), Jupyter ZeroMQ kernels (`commands::run_cell`), Python/Deno bootstrap assets.

**Decision note:** This plan implements the spec's stated decision (uniform per-session execute injection — "B"). The spec's open question #1 (uniform-B vs Databricks-style hybrid) is resolved to **B** for this plan. If the hybrid is later chosen, this plan is superseded.

---

### Task 1: Port helper bodies as checked-in assets with env-based root

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src-tauri/src/assets/ports_bootstrap.py`
- Create: `crates/spur-notebook/jute-notebook/src-tauri/src/assets/ports_bootstrap.js`
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/ports.rs` (`python_bootstrap`, `javascript_bootstrap`, the `#[cfg(test)]` round-trip tests)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ports_bootstrap.py` is the existing `_Spur` body moved verbatim, with two changes: (a) the constructor reads its root from `os.environ["SPUR_NOTEBOOK_PORT_ROOT"]` instead of an injected literal; (b) the trailing instantiation is guarded and env-driven: `if "spur" not in globals():\n    spur = _Spur(os.environ["SPUR_NOTEBOOK_PORT_ROOT"])`. `PORT_MIME` stays a literal constant inside the asset.
- [ ] `ports_bootstrap.js` is the existing `globalThis.spur` body moved verbatim, with root from `Deno.env.get("SPUR_NOTEBOOK_PORT_ROOT")` and an idempotent install: `globalThis.spur ??= new _Spur({ root: ... })`.
- [ ] `python_bootstrap()` / `javascript_bootstrap()` become `pub fn ... () -> &'static str` returning `include_str!("assets/ports_bootstrap.{py,js}")` (no `notebook_root` arg, no interpolation). `notebook_id_for_path`, `notebook_port_root`, `PORT_MIME` are unchanged.
- [ ] `python_helper_round_trips_arrow_and_emits_display_mirror` and the JS equivalent pass by setting `SPUR_NOTEBOOK_PORT_ROOT` in the test subprocess env instead of passing a root literal.
- [ ] `cargo test -p jute ports` green; `cargo build -p jute` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the two asset files, the two bootstrap loader fns, and the two `ports.rs` round-trip tests.
- OUT: `wrap_python_cell` / `wrap_js_cell` (Task 4), `wrap_cell_for_kernel` (Task 4), any kernel-launch code (Task 2/3). To keep this task compiling, leave `wrap_python_cell`/`wrap_js_cell` in place but have them call `python_bootstrap().to_string()` / `javascript_bootstrap().to_string()` (they keep prepending the static asset until Task 4 deletes them).
- If you must touch launch/dispatch code, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1 — Move bodies to assets.** Copy the current `format!(...)` template body of `python_bootstrap` into `assets/ports_bootstrap.py`, un-escaping the doubled `{{`/`}}` back to single braces. Replace the interpolated `spur = _Spur({root})` tail with:

```python
import os as _spur_os
if "spur" not in globals():
    spur = _Spur(_spur_os.environ["SPUR_NOTEBOOK_PORT_ROOT"])
```

Do the same for `assets/ports_bootstrap.js`, replacing the `__SPUR_ROOT__`/`__SPUR_PORTS_DIR__`/`__SPUR_MANIFEST_PATH__` substitutions with values derived at runtime from `Deno.env.get("SPUR_NOTEBOOK_PORT_ROOT")`, and the final `globalThis.spur = new _Spur(...)` with `globalThis.spur ??= new _Spur(...)`.

- [ ] **Step 2 — Loaders.** Replace the generator bodies in `ports.rs`:

```rust
/// Static Python port-helper bootstrap. Reads its root from SPUR_NOTEBOOK_PORT_ROOT.
pub fn python_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.py")
}

/// Static JavaScript/Deno port-helper bootstrap. Reads its root from SPUR_NOTEBOOK_PORT_ROOT.
pub fn javascript_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.js")
}
```

- [ ] **Step 3 — Keep wrappers compiling** (temporary; removed in Task 4): `wrap_python_cell` → `let mut wrapped = python_bootstrap().to_string();` etc.
- [ ] **Step 4 — Update round-trip tests** to set the env var, e.g. `Command::new("python3").env("SPUR_NOTEBOOK_PORT_ROOT", dir.path()).arg("-c").arg(script)`, and drop the interpolated-root assertions.
- [ ] **Step 5 — Run:** `cargo test -p jute ports -- --nocapture`; expect green. **Commit.**

---

### Task 2: Inject `SPUR_NOTEBOOK_PORT_ROOT` into the kernel env at spawn

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (`start_local_kernel` + its callers `start_kernel`, `restart_kernel`, `ensure_kernel_slot_live`; add a `notebook_path_from_slot_id` helper)
- Modify: `crates/spur-notebook/src/mcp/tools/start_kernel.rs`, `crates/spur-notebook/src/mcp/tools/restart_kernel.rs` (call-site updates)
- Modify: `crates/spur-notebook/tests/notebook_read_tools.rs` (4 `start_local_kernel(...)` call sites)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/local.rs` (test `kernel_command_applies_spec_env_and_preserves_parent_env`)

**Depends on:** none (parallel with Task 1; uses existing `notebook_port_root`)

**Acceptance Criteria:**
- [ ] `start_local_kernel` gains a `port_root: Option<&std::path::Path>` parameter and, when `Some`, inserts `kernel_spec.env.insert("SPUR_NOTEBOOK_PORT_ROOT".into(), root.display().to_string())` before `LocalKernel::start`.
- [ ] `ensure_kernel_slot_live` gains a `notebook_path: &str` parameter; `run_cell_events` (its only caller, already holding `notebook_path`) passes it; it computes `notebook_port_root(notebook_path)` and forwards `Some(&root)`.
- [ ] `start_kernel` resolves the current notebook path (`load_current_notebook_path_normalized().await?`) and forwards it as the port root (or `None` if absent).
- [ ] `restart_kernel` derives the notebook path from `slot_id` via a new `notebook_path_from_slot_id(slot_id) -> Option<String>` (strip `NOTEBOOK_SLOT_PREFIX` prefix and `#<spec>` suffix), else `None`.
- [ ] The two MCP tool call sites and the 4 test call sites compile (pass `None` for tests unless a path is asserted).
- [ ] New unit test `start_local_kernel_injects_port_root_env` (or extend the `kernel_command` env test) asserts the spawned spec env contains `SPUR_NOTEBOOK_PORT_ROOT == notebook_port_root(path)` and parent env is preserved.
- [ ] `cargo build -p jute` and `cargo build -p spur-notebook` green; `cargo test -p jute kernel` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the `start_local_kernel` signature + the listed call sites + the slot→path helper + the env test.
- OUT: the silent bootstrap injection (Task 3), per-cell wrap removal (Task 4), asset files (Task 1).
- If threading reveals an additional `start_local_kernel` caller not listed, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1 — Failing test** in `backend/local.rs` tests:

```rust
#[test]
fn kernel_command_includes_injected_port_root() {
    let mut env = std::collections::BTreeMap::new();
    env.insert("SPUR_NOTEBOOK_PORT_ROOT".to_string(), "/tmp/nb-x/ports-root".to_string());
    let cmd = kernel_command(&["python".to_string()], &env);
    let envs: std::collections::HashMap<_, _> = cmd.as_std().get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
        .collect();
    assert_eq!(envs.get("SPUR_NOTEBOOK_PORT_ROOT").map(String::as_str), Some("/tmp/nb-x/ports-root"));
}
```

- [ ] **Step 2 — Run:** `cargo test -p jute kernel_command_includes_injected_port_root`; expect PASS already (proves the `.envs` mechanism) — this is the contract the injection relies on.
- [ ] **Step 3 — Thread the param.** Change `start_local_kernel(spec_name: &str)` → `start_local_kernel(spec_name: &str, port_root: Option<&std::path::Path>)`; after the existing `kernel_spec` clone+argv fixup add:

```rust
if let Some(root) = port_root {
    kernel_spec.env.insert("SPUR_NOTEBOOK_PORT_ROOT".to_string(), root.display().to_string());
}
```

- [ ] **Step 4 — Update callers** (`run_cell_events` → `ensure_kernel_slot_live(state, &dispatch.slot_id, &dispatch.spec_name, dispatch.code_type, notebook_path)`; inside, `let root = notebook_port_root(notebook_path); start_local_kernel(spec_name, Some(&root)).await?`). `start_kernel`/`restart_kernel`/MCP tools/tests per acceptance criteria.
- [ ] **Step 5 — Run** `cargo build` + `cargo test -p jute kernel`; green. **Commit.**

---

### Task 3: Inject the port bootstrap once per kernel session

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (add `inject_port_bootstrap`; call it after a fresh kernel is started in `ensure_kernel_slot_live`, `start_kernel`, `restart_kernel`)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/error.rs` (or wherever `Error` lives) — add a `PortBootstrapFailed { stage: &'static str, cause: String }` variant

**Depends on:** `task-1`, `task-2`

**Acceptance Criteria:**
- [ ] `async fn inject_port_bootstrap(conn: &<conn type>, spec_name: &str) -> Result<(), Error>` selects the asset (`"deno" => javascript_bootstrap()`, else `python_bootstrap()`), runs it via `commands::run_cell(conn, src)`, drains the returned receiver, and maps any execute-error event to `Error::PortBootstrapFailed`.
- [ ] It is invoked exactly where a *fresh* kernel becomes live: in `ensure_kernel_slot_live`'s `Missing | Empty` branch (before/after `install_kernel_in_slot`, using `kernel.conn()`), and once each in `start_kernel` and `restart_kernel` after `start_local_kernel`. It is NOT invoked per cell and NOT when a slot is already `Live`.
- [ ] A failed bootstrap returns `PortBootstrapFailed` and the kernel is killed/not installed (no half-provisioned slot).
- [ ] Integration test: starting a slot then running two trivial cells shows `spur` defined in both, and a sentinel (e.g. a file the bootstrap touches once) proves single execution.
- [ ] Restart test: `restart_kernel` then a cell confirms `spur` is defined again.
- [ ] `cargo test -p spur-notebook` (notebook_read_tools) green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `inject_port_bootstrap` + its 3 call sites + the new `Error` variant.
- OUT: removing per-cell wrap (Task 4 — leave `wrap_cell_for_kernel` in place; cells still wrapped here, which is harmless/idempotent until Task 4). Asset bodies (Task 1), env threading (Task 2).
- Emit `scope_drift` if the conn type / `commands::run_cell` signature forces touching the jupyter protocol module.

**Implementation:**

- [ ] **Step 1 — Failing integration test** in `crates/spur-notebook/tests/notebook_read_tools.rs` modeled on `run_cell_collects_events_against_in_process_kernel_mock`: start a python slot, run `print("spur" in dir())`-style probe, assert truthy.
- [ ] **Step 2 — Run:** expect FAIL (spur undefined without per-cell wrap path in the mock).
- [ ] **Step 3 — Implement** `inject_port_bootstrap`:

```rust
async fn inject_port_bootstrap(conn: &KernelConnection, spec_name: &str) -> Result<(), Error> {
    let src = if spec_name == "deno" { javascript_bootstrap() } else { python_bootstrap() };
    let rx = commands::run_cell(conn, src).await?;
    // Reuse the existing drain helper / inspect events; map an execute_error to:
    //   Error::PortBootstrapFailed { stage: "execute", cause }
    drain_for_errors(rx).await.map_err(|cause| Error::PortBootstrapFailed { stage: "execute", cause })
}
```

(Use the existing `drain_run_cell_events` pattern from `src/mcp/tools/run_cell.rs` as the reference for detecting error events; `KernelConnection` = the type returned by `kernel.conn()` / `kernel_connection_for_slot`.)

- [ ] **Step 4 — Wire** into the three fresh-start sites per acceptance criteria.
- [ ] **Step 5 — Run** integration + restart tests; green. **Commit.**

---

### Task 4: Remove per-cell wrapping — cells run verbatim

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (delete `wrap_cell_for_kernel`; `resolve_run_cell_dispatch` sets `wrapped_code: code.to_string()`; update `run_cell_chokepoint_*` tests)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/ports.rs` (delete `wrap_python_cell`, `wrap_js_cell` + their tests)
- Modify: `crates/spur-notebook/src/dag/inject.rs` and `crates/spur-notebook/src/dag/mod.rs` (drop `wrap_python_cell` / `wrap_js_cell` from the re-export lists)

**Depends on:** `task-3`

**Acceptance Criteria:**
- [ ] `wrap_cell_for_kernel`, `wrap_python_cell`, `wrap_js_cell` no longer exist; no references remain (`rg wrap_python_cell` / `wrap_js_cell` / `wrap_cell_for_kernel` returns only this plan/spec/docs).
- [ ] `resolve_run_cell_dispatch` produces `wrapped_code == code` (verbatim). The `RunCellDispatch.wrapped_code` field MAY be renamed to `code` (optional, keep diff small if it ripples).
- [ ] `run_cell_chokepoint_wraps_python_code_with_port_bootstrap` / `..._deno_...` / `..._wraps_raw_code_once` are replaced by a `run_cell_chokepoint_passes_user_code_verbatim` test asserting the dispatched code equals the input and contains no `_Spur`.
- [ ] `run_cell_chokepoint_uses_same_port_root_for_same_notebook_across_specs` is re-pointed at `notebook_port_root(notebook_path)` (the env value) rather than the wrapped-string literal, OR removed if redundant with Task 2's env test (justify in commit).
- [ ] `cargo build` workspace-wide green; `cargo test -p jute` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: deletion of the three wrap fns, the dispatch change, the re-export edits, and the affected tests.
- OUT: asset bodies, env threading, injection logic (Tasks 1–3 own them).
- Emit `scope_drift` if removing the re-exports breaks an unlisted consumer.

**Implementation:**

- [ ] **Step 1 — Replace tests** in `commands.rs`:

```rust
#[test]
fn run_cell_chokepoint_passes_user_code_verbatim() {
    let code = "spur.put('sales', [1, 2])";
    // resolve_run_cell_dispatch requires State; assert via the dispatch field on a
    // constructed RunCellDispatch path, or factor wrapped_code = code directly.
    assert_eq!(code, code); // replace with real dispatch assertion once wrap is gone
}
```

(Worker: implement against the real `resolve_run_cell_dispatch` once `wrap_cell_for_kernel` is deleted; assert the emitted code has no `class _Spur`.)

- [ ] **Step 2 — Delete** `wrap_cell_for_kernel` and set `wrapped_code: code.to_string()` in `resolve_run_cell_dispatch`.
- [ ] **Step 3 — Delete** `wrap_python_cell`/`wrap_js_cell` from `ports.rs` and their tests; drop them from `dag/inject.rs` + `dag/mod.rs` re-exports.
- [ ] **Step 4 — Run** `cargo build` + `cargo test -p jute`; green. **Commit.**

---

### Task 5: Update the port contract doc

**Task ID:** `task-5`

**Files:**
- Modify: `docs/architecture/notebook-port-contract.md`

**Depends on:** `task-4`

**Acceptance Criteria:**
- [ ] The "Implementations" / per-cell-wrap description is replaced by: helper bodies live in `assets/ports_bootstrap.{py,js}`; the root is supplied via `SPUR_NOTEBOOK_PORT_ROOT`; the helper is injected once per kernel session via a silent `execute_request`.
- [ ] The "On-Disk Layout", "Manifest Shape", "Schema Authority", and "Write Protocol" sections are unchanged (the on-disk contract is frozen).
- [ ] File:line citations that moved are updated to symbol-name references (the worktree line numbers drift).
- [ ] Doc references the spec: `docs/superpowers/specs/2026-06-02-notebook-port-integration-design.ipynb`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `notebook-port-contract.md` only.
- OUT: code. Doc-only task.

**Implementation:**

- [ ] **Step 1** — Edit the Implementations + Change Checklist sections to describe assets + per-session injection.
- [ ] **Step 2** — Verify the frozen sections are untouched. **Commit.**

---

## Dependency DAG

```
task-1 (assets+loaders) ─┐
task-2 (env binding) ────┼─→ task-3 (session injection) ─→ task-4 (remove wrap) ─→ task-5 (doc)
```

task-1 and task-2 are independent roots (parallel). task-3 joins both. task-4 and task-5 are linear after.

## Self-Review

- **Spec coverage:** seams 1→task-1, 3+4→task-2, 5→task-3, 2→task-4, 6→task-5; testing strategy distributed across tasks 1–4. ✓
- **Placeholders:** the two `// replace with real ...` notes in Task 4 Step 1 are explicit worker instructions tied to "implement against real `resolve_run_cell_dispatch` once wrap is deleted," not unfilled TBDs.
- **Type consistency:** `SPUR_NOTEBOOK_PORT_ROOT`, `port_root: Option<&Path>`, `inject_port_bootstrap`, `PortBootstrapFailed` used consistently across tasks 2–4. ✓
- **DAG:** acyclic; roots parallel; chain depth 4. ✓
- **beads compatibility:** every task has unique id, explicit depends_on, verifiable acceptance criteria, scope boundary. ✓

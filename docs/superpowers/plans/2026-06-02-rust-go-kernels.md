# Rust (evcxr) + Go (gonb) Kernels Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-02-rust-go-kernels-design.ipynb`
**Design epic:** _(brainstorming design; not tracked as a separate beads epic)_

**Goal:** Add Rust (evcxr) and Go (gonb) Jupyter kernels to spur-notebook with full SPUR ports parity (cross-language `spur.put`/`spur.get` over a shared Arrow file + manifest).

**Architecture:** Fill seven existing seams. Rust is half-wired (`CodeType::Rust` + `kernelspec_for` exist; provisioning/DAG/wrap/frontend block it); Go is absent. Each task mirrors an existing Python/Deno reference implementation. Same-file work is sequenced via `depends_on` so overlays apply cleanly.

**Tech Stack:** Rust (Tauri backend, `jute-notebook/src-tauri`), TypeScript/React frontend, evcxr Jupyter kernel, gonb (`github.com/janpfeifer/gonb`), Apache Arrow IPC (`arrow` crate / `arrow-go/v18`).

---

## Reference implementations to mirror (read these first)

Every task mirrors code that already works. Workers MUST read the cited reference before writing:

- **Provisioning:** `ensure_deno_kernelspec` / `ensure_python3_kernelspec` in `crates/spur-notebook/jute-notebook/src-tauri/src/kernel_provision.rs` (direct-write `kernel.json`, `kernelspec_is_valid`, `find_binary_on_path`, env-override→PATH resolution, per-kernel `Mutex` lock, `Error::KernelProvisionFailed { stage, cause }`).
- **Ports shim:** `python_bootstrap` / `javascript_bootstrap` + `wrap_python_cell` / `wrap_js_cell` in `crates/spur-notebook/jute-notebook/src-tauri/src/ports.rs` (the `_Spur`/`spur` preamble; Arrow IPC file at `<root>/ports/<port>@v<version>.arrow`; `ports/manifest.json`; `PORT_MIME`).
- **MCP gate:** `provisioning_target_for_spec` + `call()` in `crates/spur-notebook/src/mcp/tools/start_kernel.rs`.
- **DAG gate:** `reject_unsupported_kernel_specs` in `crates/spur-notebook/src/dag/engine.rs:684-699`.
- **Frontend:** `SupportedKernelSpecName` in `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:40-44`.

**DAG of tasks** (roots run in parallel):

```
T1 enum/maps ─────────────► T7 DAG gate
T2 provisioning(evcxr+gonb) ► T5 MCP gate
T3 ports: shared+rust ─────► T4 ports: go ──► T6 wrap dispatch
T8 frontend (independent)
T1..T8 ─────────────────────► T9 integration
```
Roots (no deps): **T1, T2, T3, T8**.

---

### Task 1: Language enum + kernelspec maps

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs:251-277` (`CodeType`, `kernelspec_for`, `code_type_for_spec`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `CodeType` gains a `Go` variant (serde `rename_all="lowercase"` → serializes as `"go"`).
- [ ] `kernelspec_for(CodeType::Go) == "gonb"`; `code_type_for_spec("gonb") == Some(CodeType::Go)`.
- [ ] Existing `Rust ↔ "evcxr"` mapping unchanged.
- [ ] `cargo test -p jute code_type` passes; `cargo build -p jute` clean; TS bindings regenerate without error (the enum has `#[ts(export)]`).

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the three items in `notebook.rs` (enum + two map fns) and a unit test module for them.
- OUT: provisioning, ports, frontend, engine. If you need to touch them, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)]` module in `notebook.rs`, or create one):

```rust
#[test]
fn go_round_trips_through_kernelspec_maps() {
    assert_eq!(kernelspec_for(CodeType::Go), "gonb");
    assert_eq!(code_type_for_spec("gonb"), Some(CodeType::Go));
    // existing mappings intact
    assert_eq!(kernelspec_for(CodeType::Rust), "evcxr");
    assert_eq!(code_type_for_spec("evcxr"), Some(CodeType::Rust));
    assert_eq!(code_type_for_spec("nope"), None);
}
```

- [ ] **Step 2: Run** `cargo test -p jute go_round_trips_through_kernelspec_maps` → FAIL (no `CodeType::Go`).

- [ ] **Step 3: Implement.** Add `Go` to the enum with a doc comment; add the arms:

```rust
// in enum CodeType { ... }
    /// Go code routed to the `gonb` kernelspec.
    Go,

// in kernelspec_for:
        CodeType::Go => "gonb",

// in code_type_for_spec:
        "gonb" => Some(CodeType::Go),
```

- [ ] **Step 4: Run** the test → PASS. Then `cargo build -p jute`.

- [ ] **Step 5: Commit** `feat(notebook): add Go code_type mapped to gonb kernelspec`.

---

### Task 2: Provisioning — `ensure_evcxr_kernelspec` + `ensure_gonb_kernelspec`

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/kernel_provision.rs` (add two `ensure_*` fns, two `Mutex` locks, two version constants, `cargo`/`go` binary resolvers; mirror `ensure_deno_kernelspec`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ensure_evcxr_kernelspec()` and `ensure_gonb_kernelspec()` are `pub async fn`s returning `Result<(), Error>`, mirroring `ensure_deno_kernelspec`.
- [ ] Both short-circuit when `~/.spur/jupyter/kernels/<name>/kernel.json` already passes `kernelspec_is_valid`.
- [ ] `cargo`/`go` resolved via `CARGO_PATH`/`GO_PATH` env then `$PATH` (reuse `find_binary_on_path` / `existing_absolute_binary`); missing toolchain → `Error::KernelProvisionFailed { stage: "cargo_path" | "go_path", .. }` with an actionable cause.
- [ ] On success each **direct-writes** `kernel.json` (no `--install`/relocate): evcxr argv `[<evcxr_jupyter>, "--control_file", "{connection_file}"]` lang `"rust"`; gonb argv `[<gonb>, "--kernel", "{connection_file}"]` lang `"go"`.
- [ ] Constants added next to `MANAGED_KERNEL_DUCKDB_VERSION`: `EVCXR_ARROW_CRATE_VERSION = "55"`, `GONB_ARROW_GO_MODULE = "github.com/apache/arrow-go/v18"` (consumed later by ports shims; define now so both files reference one source of truth).
- [ ] Per-kernel locks `EVCXR_KERNELSPEC_LOCK`, `GONB_KERNELSPEC_LOCK`.
- [ ] Unit tests pass with an injected runner / env-override (no real toolchain needed). `cargo test -p jute kernel_provision` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `kernel_provision.rs` only.
- OUT: `start_kernel.rs` (Task 5 wires the MCP gate), ports, enum. Emit `scope_drift` if the install command genuinely cannot be made idempotent without touching other files.

**Implementation:**

- [ ] **Step 1: Failing test** — mirror `ensure_deno_kernelspec_writes_template_from_env_override`. Point a fake `cargo` binary via `CARGO_PATH`, run an installer-free variant by extracting the writer into a testable `ensure_evcxr_kernelspec_in_dir(spur_jupyter, evcxr_binary)` helper (mirror the `_in_dir` split already used for deno/python):

```rust
#[tokio::test]
async fn ensure_evcxr_kernelspec_writes_template_from_resolved_binary() {
    let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
    let evcxr = root.join("bin").join(if cfg!(windows) { "evcxr_jupyter.exe" } else { "evcxr_jupyter" });
    tokio::fs::create_dir_all(evcxr.parent().unwrap()).await.unwrap();
    tokio::fs::write(&evcxr, b"").await.unwrap();

    ensure_evcxr_kernelspec_in_dir(&root, &evcxr).await.unwrap();

    let kernelspec = root.join("kernels").join("evcxr").join("kernel.json");
    let spec: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&kernelspec).await.unwrap()).unwrap();
    assert_eq!(spec["language"], "rust");
    assert_eq!(spec["argv"][0], evcxr.to_string_lossy());
    assert_eq!(spec["argv"][1], "--control_file");
    assert!(kernelspec_is_valid(&kernelspec).await);
    tokio::fs::remove_dir_all(root).await.unwrap();
}
```
Add the analogous `ensure_gonb_kernelspec_writes_template_from_resolved_binary` (lang `"go"`, argv[1] `"--kernel"`).

- [ ] **Step 2: Run** `cargo test -p jute ensure_evcxr_kernelspec_writes_template` → FAIL.

- [ ] **Step 3: Implement**, mirroring `ensure_deno_kernelspec` / `ensure_deno_kernelspec_in_dir` exactly. Structure:
  - `pub async fn ensure_evcxr_kernelspec()` — lock `EVCXR_KERNELSPEC_LOCK`, resolve `spur_jupyter_dir()`, resolve `cargo` (`cargo_binary_path()` mirroring `deno_binary_path()` but env `CARGO_PATH`), run `cargo install --locked evcxr_jupyter` via `tokio::process::Command` (reuse `format_command_failure`), locate `evcxr_jupyter` (`~/.cargo/bin` then `find_binary_on_path("evcxr_jupyter")`), then `ensure_evcxr_kernelspec_in_dir(&spur_jupyter, &evcxr)`.
  - `ensure_evcxr_kernelspec_in_dir(spur_jupyter, evcxr) -> Result<(), Error>` — validity short-circuit, `create_dir_all`, write the JSON payload (mirror the deno `serde_json::json!` writer), re-validate.
  - Generalize `deno_binary_path`'s helper or add `resolve_toolchain(env_var, bin, stage)` so cargo/go reuse it.
  - gonb analog: env `GO_PATH`, `go install github.com/janpfeifer/gonb@latest`, locate `gonb` in `$GOBIN`/`$GOPATH/bin` then PATH, `ensure_gonb_kernelspec_in_dir`.
  - Add the two `const` lines under line 16 and the two `static ... Mutex` locks under line 13.

- [ ] **Step 4: Run** `cargo test -p jute kernel_provision` → PASS; `cargo build -p jute`.

- [ ] **Step 5: Commit** `feat(notebook): provision evcxr and gonb kernelspecs (detect-and-install)`.

**Scope Drift Checkpoint:** if `cargo install`/`go install` output parsing forces touching files outside `kernel_provision.rs`, emit `scope_drift`.

---

### Task 3: Ports shim — shared format helpers + Rust (`rust_bootstrap`, `wrap_rust_cell`)

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/ports.rs` (factor shared format constants; add `rust_bootstrap`, `wrap_rust_cell`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] Shared format facts are defined once (the `ports/` subdir name, the `<port>@v<version>.arrow` filename scheme, the `manifest.json` shape/version-bump rule, port-name validation, and the existing `PORT_MIME`) and referenced by `python_bootstrap`/`javascript_bootstrap`/`rust_bootstrap` rather than re-spelled per language. No behavior change to Python/JS bootstraps (their existing tests stay green).
- [ ] `pub fn rust_bootstrap(notebook_root: impl AsRef<Path>) -> String` emits evcxr `:dep arrow = "<EVCXR_ARROW_CRATE_VERSION>"` + `:dep serde_json = "1"` then a `spur` helper with `put`/`get` over `arrow::ipc::writer::FileWriter` / `reader::FileReader`, writing the identical path scheme + manifest + `PORT_MIME` display payload as `python_bootstrap`.
- [ ] `pub fn wrap_rust_cell(root, code) -> String` prepends the bootstrap (mirror `wrap_python_cell`).
- [ ] `cargo test -p jute ports` green (existing + new). New test asserts `rust_bootstrap` contains the `:dep arrow` line, the `ports/` path scheme, and `PORT_MIME`.

**Suggested Worker:** claude-code-acp (Arrow-in-Rust shim is judgment-heavy; multi-function single file)

**Scope Boundary:**
- IN: `ports.rs`.
- OUT: `commands.rs` dispatch (Task 6), Go shim (Task 4), `src/dag/inject.rs` re-exports may be extended in Task 6. Emit `scope_drift` if the manifest format must change in a way that would alter Python/JS output bytes.

**Implementation:**

- [ ] **Step 1: Failing test:**

```rust
#[test]
fn rust_bootstrap_pulls_arrow_and_uses_shared_port_paths() {
    let src = rust_bootstrap("/tmp/nb-root");
    assert!(src.contains(":dep arrow ="));
    assert!(src.contains("ports/"));            // shared subdir
    assert!(src.contains(PORT_MIME));           // same display MIME as Python/JS
    let wrapped = wrap_rust_cell(std::path::PathBuf::from("/tmp/nb-root"), "let x = 1;");
    assert!(wrapped.ends_with("let x = 1;"));
    assert!(wrapped.contains(":dep arrow ="));
}
```

- [ ] **Step 2: Run** `cargo test -p jute rust_bootstrap_pulls_arrow` → FAIL.

- [ ] **Step 3: Implement.** Read `python_bootstrap` (line 26) end-to-end; replicate its manifest/version/path logic as a Rust source string. Pull `EVCXR_ARROW_CRATE_VERSION` from `kernel_provision` (re-export or duplicate the `const` with a shared `pub const` in `ports.rs` referenced by `kernel_provision`; pick one home and reference it — do not define two diverging values). The emitted `spur.put` must: validate port name, read+bump `manifest.json`, write `ports/<port>@v<version>.arrow` via Arrow IPC **file** format, emit the `PORT_MIME` payload. `spur.get` reads the manifest's current version and loads that `.arrow`.

- [ ] **Step 4: Run** `cargo test -p jute ports` → PASS.

- [ ] **Step 5: Commit** `feat(notebook): rust ports bootstrap + shared port-format helpers`.

---

### Task 4: Ports shim — Go (`go_bootstrap`, `wrap_go_cell`)

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/ports.rs` (add `go_bootstrap`, `wrap_go_cell`, reusing Task 3's shared helpers)

**Depends on:** `task-3` (same file; reuses the shared format helpers)

**Acceptance Criteria:**
- [ ] `pub fn go_bootstrap(notebook_root) -> String` emits a gonb preamble that pulls `<GONB_ARROW_GO_MODULE>` and defines `spur.Put`/`spur.Get` over the identical Arrow IPC file + manifest + `PORT_MIME` contract.
- [ ] `pub fn wrap_go_cell(root, code) -> String` prepends it (mirror `wrap_js_cell`).
- [ ] `cargo test -p jute ports` green; new test asserts `go_bootstrap` references the arrow-go module, the `ports/` scheme, and `PORT_MIME`.

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN: `ports.rs` (new Go functions + shared-helper reuse).
- OUT: don't modify `rust_bootstrap`/`python_bootstrap` bodies. Emit `scope_drift` if shared helpers from Task 3 are insufficient and need reshaping.

**Implementation:**

- [ ] **Step 1: Failing test:**

```rust
#[test]
fn go_bootstrap_pulls_arrow_go_and_uses_shared_port_paths() {
    let src = go_bootstrap("/tmp/nb-root");
    assert!(src.contains("arrow-go/v18"));
    assert!(src.contains("ports/"));
    assert!(src.contains(PORT_MIME));
    let wrapped = wrap_go_cell(std::path::PathBuf::from("/tmp/nb-root"), "x := 1");
    assert!(wrapped.ends_with("x := 1"));
}
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement**, mirroring `javascript_bootstrap` structure but emitting Go using gonb's `!*go get` / import conventions and the arrow-go IPC reader/writer. Same path/manifest/MIME contract as Task 3.
- [ ] **Step 4: Run** `cargo test -p jute ports` → PASS.
- [ ] **Step 5: Commit** `feat(notebook): go ports bootstrap (gonb + arrow-go)`.

---

### Task 5: MCP provisioning gate

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/start_kernel.rs` (`KernelspecProvisioningTarget`, `provisioning_target_for_spec`, `call()`; update `evcxr_*` tests)

**Depends on:** `task-2` (needs `ensure_evcxr_kernelspec`/`ensure_gonb_kernelspec`)

**Acceptance Criteria:**
- [ ] `KernelspecProvisioningTarget` gains `Evcxr` and `Gonb`; `NotYetSupported` removed.
- [ ] `provisioning_target_for_spec`: `"deno"⇒Deno`, `"evcxr"⇒Evcxr`, `"gonb"⇒Gonb`, `_⇒Python3`.
- [ ] `call()` dispatches `Evcxr⇒ensure_evcxr_kernelspec()`, `Gonb⇒ensure_gonb_kernelspec()` (neither needs the `AppHandle`, unlike Python3).
- [ ] The two existing tests `evcxr_spec_reports_not_yet_supported_provisioning` and `evcxr_start_kernel_returns_not_supported_signal` are replaced by tests asserting `provisioning_target_for_spec("evcxr") == Evcxr` and `("gonb") == Gonb`.
- [ ] `cargo test -p spur-notebook start_kernel` green; `cargo build` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `start_kernel.rs`.
- OUT: `kernel_provision.rs` (Task 2 owns the `ensure_*`), engine, run_cell/run_cascade. Emit `scope_drift` otherwise.

**Implementation:**

- [ ] **Step 1: Replace the failing assertions:**

```rust
#[test]
fn evcxr_and_gonb_map_to_their_provisioning_targets() {
    assert_eq!(provisioning_target_for_spec("evcxr"), KernelspecProvisioningTarget::Evcxr);
    assert_eq!(provisioning_target_for_spec("gonb"), KernelspecProvisioningTarget::Gonb);
    assert_eq!(provisioning_target_for_spec("deno"), KernelspecProvisioningTarget::Deno);
    assert_eq!(provisioning_target_for_spec("python3"), KernelspecProvisioningTarget::Python3);
}
```
Delete `evcxr_spec_reports_not_yet_supported_provisioning` and `evcxr_start_kernel_returns_not_supported_signal`.

- [ ] **Step 2: Run** `cargo test -p spur-notebook start_kernel` → FAIL (variant missing).
- [ ] **Step 3: Implement.** Add the enum variants; update the match; in `call()` add:

```rust
KernelspecProvisioningTarget::Evcxr => ensure_evcxr_kernelspec().await,
KernelspecProvisioningTarget::Gonb => ensure_gonb_kernelspec().await,
```
Import them: `use jute::kernel_provision::{ensure_deno_kernelspec, ensure_evcxr_kernelspec, ensure_gonb_kernelspec, ensure_python3_kernelspec};`. Remove the `NotYetSupported` arm.

- [ ] **Step 4: Run** → PASS; `cargo build`.
- [ ] **Step 5: Commit** `feat(notebook): provision evcxr/gonb from start_kernel MCP tool`.

---

### Task 6: Cell-wrap 4-way dispatch

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs:1713` (`wrap_cell_for_kernel`)
- Modify (if needed): `crates/spur-notebook/src/dag/inject.rs` + `src/dag/mod.rs` re-exports to surface `wrap_rust_cell`/`wrap_go_cell`

**Depends on:** `task-3`, `task-4`

**Acceptance Criteria:**
- [ ] `wrap_cell_for_kernel` becomes a 4-way match: `"deno"⇒wrap_js_cell`, `"evcxr"⇒wrap_rust_cell`, `"gonb"⇒wrap_go_cell`, `_⇒wrap_python_cell`.
- [ ] `ports::{wrap_rust_cell, wrap_go_cell}` are imported (and re-exported through `src/dag/inject.rs` if other call sites need them).
- [ ] `cargo test -p jute wrap_cell` green; existing Python/Deno wrap tests unchanged.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the `wrap_cell_for_kernel` fn + its imports + dag re-export lines.
- OUT: the bootstrap bodies (Tasks 3/4 own them). Emit `scope_drift` otherwise.

**Implementation:**

- [ ] **Step 1: Failing test** (extend the existing wrap tests in `commands.rs`):

```rust
#[test]
fn wrap_cell_for_kernel_routes_by_spec() {
    let rs = wrap_cell_for_kernel("/tmp/demo.ipynb", "evcxr", "let x = 1;");
    assert!(rs.contains(":dep arrow ="));
    let go = wrap_cell_for_kernel("/tmp/demo.ipynb", "gonb", "x := 1");
    assert!(go.contains("arrow-go/v18"));
    let py = wrap_cell_for_kernel("/tmp/demo.ipynb", "python3", "spur.get('a')");
    assert!(py.contains("_Spur"));
}
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement:**

```rust
fn wrap_cell_for_kernel(notebook_path: &str, spec_name: &str, code: &str) -> String {
    let root = notebook_port_root(notebook_path);
    match spec_name {
        "deno" => wrap_js_cell(root, code),
        "evcxr" => wrap_rust_cell(root, code),
        "gonb" => wrap_go_cell(root, code),
        _ => wrap_python_cell(root, code),
    }
}
```
Update the `use ...ports::{...}` import at the top of `commands.rs` to include `wrap_rust_cell, wrap_go_cell`.

- [ ] **Step 4: Run** → PASS; `cargo build -p jute`.
- [ ] **Step 5: Commit** `feat(notebook): route cell wrap to rust/go bootstraps by kernelspec`.

---

### Task 7: Open the DAG gate

**Task ID:** `task-7`

**Files:**
- Modify: `crates/spur-notebook/src/dag/engine.rs:684-699` (`reject_unsupported_kernel_specs`) and its tests (~`:1370-1401`, `:1377`)

**Depends on:** `task-1` (Go tests use `CodeType::Go`)

**Acceptance Criteria:**
- [ ] `reject_unsupported_kernel_specs` no longer rejects `"evcxr"` or `"gonb"` (define the supported set as `{python3, deno, evcxr, gonb}`; reject anything else — keep the guard for genuinely-unknown specs).
- [ ] The test `run_cell_and_cascade_fails_fast_for_rust_cells_before_preflight_or_dispatch` is repurposed: a Rust cell now proceeds past the gate (assert no `UnsupportedKernelspec` for `evcxr`); add an analogous Go-cell case; add a negative case that an unknown spec (e.g. `"ruby"`) still yields `UnsupportedKernelspec`.
- [ ] `cargo test -p spur-notebook engine` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `engine.rs` (the gate fn + the cited tests).
- OUT: `notebook_run_cell.rs` / `notebook_run_cascade.rs` (error translators — do NOT modify; their hand-built `UnsupportedKernelspec` tests stay). Emit `scope_drift` otherwise.

**Implementation:**

- [ ] **Step 1: Adjust the test** to expect support for evcxr/gonb and rejection only for unknown specs:

```rust
const SUPPORTED: &[&str] = &["python3", "deno", "evcxr", "gonb"];
// rust cell no longer fails fast at the gate:
let result = engine.run_cell_and_cascade("a").await;
assert!(!matches!(result, Err(EngineError::UnsupportedKernelspec { ref spec_name, .. }) if spec_name == "evcxr"));
```

- [ ] **Step 2: Run** `cargo test -p spur-notebook engine` → FAIL.
- [ ] **Step 3: Implement** — invert the filter to a supported-set membership check:

```rust
fn reject_unsupported_kernel_specs(requirements: &[KernelRequirement]) -> Result<(), EngineError> {
    const SUPPORTED: &[&str] = &["python3", "deno", "evcxr", "gonb"];
    for requirement in requirements {
        if !SUPPORTED.contains(&requirement.spec_name.as_str()) {
            return Err(EngineError::UnsupportedKernelspec {
                spec_name: requirement.spec_name.clone(),
                cell_ids: requirement.cell_ids.clone(),
            });
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(notebook): allow evcxr and gonb through the DAG kernelspec gate`.

---

### Task 8: Frontend supported-kernelspec type

**Task ID:** `task-8`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts:40-44` (`SupportedKernelSpecName`, `supportedKernelSpecName`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `type SupportedKernelSpecName = "deno" | "python3" | "evcxr" | "gonb"`.
- [ ] `supportedKernelSpecName` returns the name when it is one of the four, else falls back to `"python3"`.
- [ ] `npm run -w jute-notebook test` (vitest) passes; `tsc`/lint clean. Add a vitest case for `evcxr`/`gonb` narrowing.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: `notebook.ts` (the type + the narrowing fn + a test).
- OUT: Rust, other stores. Emit `scope_drift` otherwise.

**Implementation:**

- [ ] **Step 1: Failing vitest** (in the notebook store test file):

```ts
import { describe, it, expect } from "vitest";
import { supportedKernelSpecName } from "./notebook";
describe("supportedKernelSpecName", () => {
  it("accepts evcxr and gonb, falls back otherwise", () => {
    expect(supportedKernelSpecName("evcxr")).toBe("evcxr");
    expect(supportedKernelSpecName("gonb")).toBe("gonb");
    expect(supportedKernelSpecName("ruby")).toBe("python3");
  });
});
```
(If `supportedKernelSpecName` is not exported, export it.)

- [ ] **Step 2: Run** `npm run -w jute-notebook test` → FAIL.
- [ ] **Step 3: Implement:**

```ts
type SupportedKernelSpecName = "deno" | "python3" | "evcxr" | "gonb";
function supportedKernelSpecName(name?: string): SupportedKernelSpecName {
  return name === "deno" || name === "python3" || name === "evcxr" || name === "gonb"
    ? name
    : "python3";
}
```

- [ ] **Step 4: Run** → PASS; `npm run -w jute-notebook lint`.
- [ ] **Step 5: Commit** `feat(notebook-ui): recognize evcxr and gonb kernelspecs`.

---

### Task 9: Cross-language ports integration test (toolchain-gated)

**Task ID:** `task-9`

**Files:**
- Create: `crates/spur-notebook/tests/rust_go_ports_e2e.rs`

**Depends on:** `task-1`, `task-2`, `task-3`, `task-4`, `task-5`, `task-6`, `task-7`, `task-8`

**Acceptance Criteria:**
- [ ] Test skips cleanly (returns early with a logged reason) when `cargo`/`go` are unavailable — mirror the existing `PYTHON_PATH`-gated e2e tests' availability guard.
- [ ] When toolchains are present: provisions evcxr + gonb, starts each kernel, runs a Python cell `spur.put('t', <table>)`, then `spur.get('t')` from a Rust cell and a Go cell asserting equal contents; then `put` from Rust/Go and `get` from Python.
- [ ] A provisioning smoke assertion that `ensure_evcxr_kernelspec` / `ensure_gonb_kernelspec` produce a startable kernel.
- [ ] `cargo test -p spur-notebook --test rust_go_ports_e2e` passes (or skips) locally.

**Suggested Worker:** claude-code-acp (multi-kernel orchestration, judgment)

**Scope Boundary:**
- IN: the new e2e test file (+ a small shared test helper if needed).
- OUT: production code — if a production gap is found, emit `scope_drift` (do NOT silently patch other tasks' files).

**Implementation:**

- [ ] **Step 1: Write the availability guard + round-trip** modeled on the existing notebook e2e tests (`tests/notebook_read_tools.rs` uses `PYTHON_PATH`/`python3` detection). Detect `cargo`/`go` via the same `CARGO_PATH`/`GO_PATH`→PATH resolution; `eprintln!` + `return` when absent.
- [ ] **Step 2: Run** `cargo test -p spur-notebook --test rust_go_ports_e2e -- --nocapture` → on a machine without Go/Rust it logs skip and passes; with toolchains it exercises the round-trip.
- [ ] **Step 3: Implement** the cross-language assertions against the running kernels.
- [ ] **Step 4: Run** → PASS/skip.
- [ ] **Step 5: Commit** `test(notebook): cross-language evcxr/gonb ports round-trip e2e`.

**Scope Drift Checkpoint:** if the round-trip reveals the Arrow/manifest format diverges between languages, emit `scope_drift` (it means Task 3/4's contract needs a fix, not a test workaround).

---

## Self-Review

**Spec coverage:** Seven seams → T1 (enum/maps), T2 (provisioning), T5 (MCP gate), T7 (DAG gate), T3+T4 (ports shims), T6 (wrap dispatch), T8 (frontend). Ports contract → T3/T4 + T9 acceptance. Pinned versions → T2 constants, consumed in T3/T4. "Not a gate" note → encoded in T7's scope boundary. All covered.

**Placeholder scan:** No TBD/TODO; every code step has concrete content or an explicit reference-to-mirror with the exact function name and file:line.

**Type consistency:** `CodeType::Go`/`"gonb"` (T1) reused by T7; `ensure_evcxr_kernelspec`/`ensure_gonb_kernelspec` (T2) imported by T5; `wrap_rust_cell`/`wrap_go_cell` (T3/T4) consumed by T6; `EVCXR_ARROW_CRATE_VERSION`/`GONB_ARROW_GO_MODULE` (T2) referenced by T3/T4 — T3 notes the single-home rule to avoid divergence.

**DAG validation:** Edges — T7←T1; T5←T2; T4←T3; T6←T3,T4; T9←all. No cycles. Roots T1,T2,T3,T8 run in parallel. Same-file pairs (T3/T4 on ports.rs; T2 alone on kernel_provision.rs) are sequenced so overlays apply cleanly.

**beads compatibility:** every task has a unique id, explicit `depends_on`, brain-verifiable acceptance criteria, and an IN/OUT scope boundary.
